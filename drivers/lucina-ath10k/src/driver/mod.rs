//! The host driver: everything from the copy engines to a station, for one kernel thread.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/core.c`, `snoc.c`, `htc.c`, `htt.c`, `htt_tx.c`, `htt_rx.c`,
//! `txrx.c`, `mac.c`, `wmi.c` and `wmi-tlv.c` (SPDX ISC; Copyright (c) 2005-2011 Atheros Communications Inc., 2011-2017
//! Qualcomm Atheros, Inc., 2018-2019 The Linux Foundation, 2022, 2024 Qualcomm Innovation Center, Inc., Qualcomm
//! Technologies, Inc. and/or its subsidiaries).
//!
//! **What this is known to be:** it agrees with Linux's encoders (the wire formats, through `oracle/`) and it runs a
//! whole station session against a model firmware that the same author wrote from reading the same C
//! (`tests/driver.rs`). **It has never met the firmware.** The order of operations follows Linux; what the firmware
//! really wants is unproven until the laptop says so.
//!
//! # Use
//!
//! The kernel loads the firmware and calls `wlan_enable` over QMI with the tables of [`crate::snoc_config`]. Then:
//!
//! ```text
//! let mut d = Driver::new(platform, Config::default())?;
//! d.start()?;
//! loop {
//!     for ev in d.poll(now_ms) { /* ... */ }   // every 1-2 ms
//! }
//! ```
//!
//! [`Event::Ready`] ends the bring-up (`ath10k_core_start`, `ath10k_start`, `ath10k_add_interface`). A station then
//! goes [`Driver::start_scan`] ... [`Event::ScanDone`], [`Driver::join`] ... [`Event::VdevStarted`],
//! [`Driver::tx_mgmt`] for auth and assoc, [`Driver::peer_assoc`], [`Driver::up`], [`Driver::tx_data`] for EAPOL with
//! `no_encrypt`, [`Driver::install_key`] twice, and data. [`Driver::leave`] undoes the join. Nothing blocks: commands
//! queue, and their outcome comes back as an [`Event`] from a later poll. A timeout that Linux would wait out becomes
//! an event too.
//!
//! # Memory
//!
//! With [`Config::default`] on the machine this runs on, DMA-able (see [`Config::dma_bytes`]):
//!
//! | what | count | bytes |
//! |---|---|---|
//! | HTT rx buffers (2048 B) | 2047 | 4 192 256 |
//! | HTT rx ring and index | 2048 x 8 + 4 | 16 388 |
//! | CE rx buffers, CE1/2/7/8/9/10/11 (2048 B) | 511 + 63 + 1 + 7 + 511 + 511 + 7 = 1611 | 3 299 328 |
//! | CE rx buffers, CE5 (512 B) | 511 | 261 632 |
//! | tx frame copies (2048 B) | 1056 | 2 162 688 |
//! | tx buffers and frag descriptors (44 + 72 B) | 1056 | 122 496 |
//! | **total before the firmware's memory requests** | | **about 10 MiB** |
//!
//! The ring arena ([`Driver::arena_bytes`]) is about 10 KiB. On top come what service ready asks for, unknown until
//! the firmware says (every chunk is capped at [`MAX_MEM_CHUNK`]), and short-lived copies of commands and management
//! frames. Each buffer takes a whole page on this machine, so about 20 MiB in all. These are Linux's sizes.
//!
//! # Where this differs from Linux, on purpose
//!
//! * It boots the firmware once; Linux does `ath10k_core_start` at probe and again at interface up. So the in-order
//!   flag, which Linux samples from the previous boot's service map, is set from this boot's map, before the first
//!   ring fill (`ath10k_htt_rx_ring_refill`, which is after the ready event).
//! * Smaller rings and pending count ([`Config`]). `num_msdu_desc` in the init command stays Linux's 1056, because it
//!   sizes a firmware pool; the frag bank offered to the firmware covers only the ids used here.
//! * Every engine is drained on every poll: see `hif.rs`.
//! * Bundled management completions use their status array, not `desc_ids[i]` as Linux does by a typo; see
//!   [`crate::wmi_tlv::parse_mgmt_tx_bundle_compl`].
//! * Power save as mac80211 and `ath10k_mac_vif_setup_ps` do it: off from join (`WMI_STA_PS_MODE_DISABLED`), then on once the AP's
//!   peer is authorized after the pairwise key, with the inactivity time first. `Config::power_save = false` keeps it off.
//! * `WMI_TLV_TX_PAUSE_EVENTID` is ignored: Linux only acts on it for multi-channel and P2P.
//! * The replay check of [`data::PnWindow`] replaces mac80211's.
//!
//! Anything from the firmware is treated as hostile: lengths are checked, loops are bounded by host-sized rings, a
//! bad message is counted in [`Stats`] and dropped. Where the C trusts it the parser docs say so.

mod data;
pub mod fw_ie;
mod hif;
mod peer;
pub mod regd;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::ce::{self, CeError};
use crate::htc::{self, Htc, svc as htc_svc};
use crate::htt::{self, T2h, TxResult};
use crate::snoc_config::CE_COUNT;
use crate::wmi_tlv::{self as wmi, EventId, cmd, sta_ps, vdev_type};

pub use data::{Decap, PnWindow, RxDrops, RxFrame, RxRate};
pub use peer::{Bandwidth, HtInfo, PeerAssoc, VhtInfo};

use data::{HttRx, HttTx, PnWindows, PpduStatus, RxCtx, TxError, TxKind};
use hif::Hif;

/// `ATH10K_DEFAULT_NOISE_FLOOR`.
pub const NOISE_FLOOR: i32 = -95;
/// `ATH10K_MAX_5G_CHAN`.
const MAX_5G_CHAN: u32 = 173;
/// `TARGET_TLV_NUM_WOW_PATTERNS`.
const NUM_WOW_PATTERNS: u32 = 22;
/// `ATH10K_HW_TXRX_NATIVE_WIFI`, for the rx decap mode and the vdev's tx encapsulation.
const TXRX_NATIVE_WIFI: u32 = 1;
/// `TARGET_HL_TLV_NUM_PEERS` and `TARGET_TLV_NUM_VDEVS` plus one, for `NUM_UNITS_IS_*` memory requests.
const PEERS_UNITS: u32 = wmi::wcn3990::NUM_PEERS + 1;
const VDEVS_UNITS: u32 = wmi::wcn3990::NUM_VDEVS + 1;
/// The one vdev: `__ffs64(free_vdev_map)` of a fresh map.
const VDEV_ID: u32 = 0;
/// `WMI_MAX_SPATIAL_STREAM`, what `ath10k_core_init_firmware_features` sets for TLV firmware.
const MAX_SPATIAL_STREAM: u32 = 3;
/// `IEEE80211_CCMP_MIC_LEN`.
const CCMP_MIC_LEN: usize = 8;

/// The largest memory chunk given to the firmware. Linux has no limit; a hostile request should not eat the machine.
pub const MAX_MEM_CHUNK: usize = 8 * 1024 * 1024;
/// Commands waiting for WMI credits, at most.
const WMI_QUEUE_MAX: usize = 96;
/// Management frames in flight (Linux allows `TARGET_TLV_MGMT_NUM_MSDU_DESC`, 50; a station sends one at a time).
const MGMT_PENDING_MAX: usize = 8;
/// The longest management frame accepted (`IEEE80211_MAX_DATA_LEN`).
const MGMT_FRAME_MAX: usize = 2304;

/// Timeouts of the C, in ms.
mod timeout {
    /// `ATH10K_HTC_WAIT_TIMEOUT_HZ`, `ATH10K_HTC_CONN_SVC_TIMEOUT_HZ`.
    pub const HTC: u64 = 1000;
    /// `WMI_SERVICE_READY_TIMEOUT_HZ`, `WMI_UNIFIED_READY_TIMEOUT_HZ`.
    pub const WMI: u64 = 5000;
    /// `HTT_TARGET_VERSION_TIMEOUT_HZ`.
    pub const HTT: u64 = 3000;
    /// `ath10k_wmi_cmd_send` ("restarting hardware"), `ath10k_install_key`, the peer waits.
    pub const CMD: u64 = 3000;
    /// `ATH10K_VDEV_SETUP_TIMEOUT_HZ`.
    pub const VDEV: u64 = 5000;
    /// `ath10k_start_scan`: the started event.
    pub const SCAN_START: u64 = 1000;
    /// `ath10k_scan_stop`.
    pub const SCAN_STOP: u64 = 3000;
}

/// A DMA buffer: mapped non-cacheable by the kernel, so that no sync is needed, and at a device address equal to the
/// physical one (SMMU stream 0xc0 is bypass).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmaBuf {
    pub id: u64,
    pub paddr: u64,
    pub len: usize,
}

/// DMA buffers from the kernel. `read` and `write` are in bounds whatever the device did: the driver's offsets come
/// from its own tables and `DmaBuf::len`.
pub trait Dma {
    /// Zeroing is not required. Must return memory the device can reach (35 address bits) and at least 4-byte aligned;
    /// `None` is handled at every call.
    fn alloc(&mut self, len: usize) -> Option<DmaBuf>;
    /// `dma_alloc_coherent`: memory the host and the device share without cache maintenance, for the structures Linux
    /// allocates that way (the HTT rx ring and its index, the tx buffer array, the frag bank, the firmware's memory
    /// chunks). Packet buffers stay on [`Dma::alloc`]: Linux maps those streaming. The default falls back to `alloc`,
    /// which the test mocks satisfy; the kernel implements it as an uncached mapping.
    fn alloc_coherent(&mut self, len: usize) -> Option<DmaBuf> {
        self.alloc(len)
    }
    fn free(&mut self, b: DmaBuf);
    fn write(&mut self, b: &DmaBuf, off: usize, data: &[u8]);
    fn read(&mut self, b: &DmaBuf, off: usize, out: &mut [u8]);
}

/// What the kernel provides. Order barriers between shared memory and doorbells are `ce::DescMem::barrier`.
pub trait Platform {
    /// The 8 MiB window at 0x18800000.
    type Regs: ce::Regs;
    /// One coherent arena for rings, by offset.
    type Mem: ce::DescMem;
    type Dma: Dma;
    fn parts(&mut self) -> (&mut Self::Regs, &mut Self::Mem, &mut Self::Dma);
    /// The arena's device address, 8-byte aligned.
    fn arena_bus_addr(&self) -> u64;
    /// The arena's length; [`Driver::arena_bytes`] says what is needed.
    fn arena_len(&self) -> usize;
}

/// Sizes and the MAC address. Each ring entry count is rounded up to a power of two by the copy engine code.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Used instead of the address in the firmware's ready event.
    pub mac: Option<[u8; 6]>,
    /// HTT rx ring entries, a power of two in 128..=2048 (`HTT_RX_RING_SIZE_MIN`, `_MAX`). Linux: 2048.
    pub htt_rx_ring_size: u16,
    /// MSDUs in flight, and tx buffers. Linux: 1056.
    pub max_pending_tx: u16,
    /// Size of one tx frame copy.
    pub tx_frame_size: usize,
    /// CE4 entries; each MSDU takes two. Linux: 2048, which is fewer than `max_pending_tx` needs: as there, a full ring stops the sender
    /// first ([`Driver::can_tx`]).
    pub ce4_entries: u32,
    /// CE1 rx entries, HTT messages. Linux: 512.
    pub ce_rx_htt: u32,
    /// CE2 rx entries, WMI events. Linux: 64.
    pub ce_rx_wmi: u32,
    /// CE5 rx entries, 512-byte HTT messages. Linux: 512.
    pub ce_rx_htt_small: u32,
    /// CE9 and CE10 rx entries, more HTT. Linux: 512.
    pub ce_rx_htt_extra: u32,
    /// CE8 and CE11 rx entries, which nothing reads. Linux: 128 and 512.
    pub ce_rx_unused: u32,
    /// How long the first message from the firmware may take, in ms. Linux: 2000 (`ath10k_htc_wait_target` waits twice).
    pub htc_ready_ms: u64,
    /// Offer VHT when the firmware and the AP have it.
    pub vht: bool,
    /// Give the firmware the AP's WMM parameters.
    pub wmm: bool,
    /// Station power save once the AP has authorized us, as Linux with `CFG80211_DEFAULT_PS`; false keeps the radio awake, as before.
    pub power_save: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mac: None,
            htt_rx_ring_size: 2048,
            max_pending_tx: 1056,
            tx_frame_size: 2048,
            ce4_entries: 2048,
            ce_rx_htt: 512,
            ce_rx_wmi: 64,
            ce_rx_htt_small: 512,
            ce_rx_htt_extra: 512,
            ce_rx_unused: 8,
            htc_ready_ms: 2 * timeout::HTC,
            vht: true,
            wmm: true,
            power_save: true,
        }
    }
}

impl Config {
    fn check(&self) -> Result<(), DriverError> {
        let s = self.htt_rx_ring_size;
        let tx = u32::from(self.max_pending_tx);
        if !s.is_power_of_two() || !(128..=2048).contains(&s) {
            return Err(DriverError::Invalid);
        }
        // the ids are 16 bits and every MSDU takes two CE4 entries, of which one stays free
        if tx == 0 || tx > 4096 || self.ce4_entries < 4 || self.tx_frame_size < 256 {
            return Err(DriverError::Invalid);
        }
        Ok(())
    }

    /// DMA memory the driver holds in steady state, before the firmware's memory requests.
    pub fn dma_bytes(&self) -> usize {
        let mut n = usize::from(self.htt_rx_ring_size) * 8 + 4;
        n += (usize::from(self.htt_rx_ring_size) - 1) * usize::from(htt::RX_BUF_SIZE);
        n += usize::from(self.max_pending_tx)
            * (self.tx_frame_size + htt::TXBUF_64_LEN + htt::MSDU_EXT_DESC_64_LEN);
        for a in &hif::host_attrs(self) {
            if a.dest_nentries != 0 {
                // one slot stays empty
                let posted = ce::ring_entries(a.dest_nentries).unwrap_or(1) as usize - 1;
                n += posted * a.src_sz_max as usize;
            }
        }
        n
    }
}

/// What the firmware told about the radio, for the association request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub ht_cap_info: u32,
    pub vht_cap_info: u32,
    pub vht_supp_mcs: u32,
    pub num_rf_chains: u32,
    pub eeprom_rd: u32,
    /// The regulatory pair in use.
    pub regdomain: u16,
}

/// Fatal for the data path; the kernel restarts the firmware (`ath10k_core_start_recovery`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A bring-up step did not finish in time.
    BootTimeout(BootStage),
    /// Refused or malformed HTC handshake.
    Htc,
    /// Service ready did not parse, or another ABI.
    ServiceReady(wmi::WmiError),
    /// `htt_target_version_major` is neither 2 nor 3.
    HttVersion(u8),
    /// Out of DMA memory during bring-up or a firmware request beyond [`MAX_MEM_CHUNK`].
    NoMem,
    /// A command got no credit in 3 s ("wmi command timeout, restarting hardware").
    WmiStall,
    /// A copy engine refused a send during bring-up.
    Ce(CeError),
    /// `rx_confused`: the rx ring and the firmware disagree.
    RxConfused,
}

/// Where the bring-up stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootStage {
    HtcReady,
    HtcConnect,
    ServiceReady,
    WmiReady,
    HttVersion,
    Settle,
}

/// Why the link is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disconnect {
    /// `WMI_TLV_PEER_STA_KICKOUT_EVENTID` for the AP.
    Kickout,
    /// [`Driver::leave`] finished.
    Left,
}

/// What came out of a poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Bring-up done, the vdev exists. `mac` is [`Config::mac`], else the firmware's.
    Ready {
        mac: [u8; 6],
        caps: Caps,
    },
    /// A management frame (`ath10k_wmi_event_mgmt_rx`), with the protected bit cleared when the firmware decrypted it.
    /// `rssi` is `snr + NOISE_FLOOR`.
    MgmtRx {
        frame: Vec<u8>,
        rssi: i32,
        freq: u32,
    },
    ScanDone {
        aborted: bool,
    },
    /// [`Driver::join`] finished: vdev started, peer mapped, frames can go.
    VdevStarted,
    /// The vdev start was refused or the peer did not appear.
    JoinFailed,
    /// [`Driver::peer_assoc`]'s commands are with the firmware (Linux waits for nothing either).
    PeerAssocDone,
    /// The key is in (`HTT_T2H_MSG_TYPE_SEC_IND`, what `ath10k_install_key` waits for). For a pairwise key the peer
    /// has been authorized too.
    KeyInstalled {
        pairwise: bool,
    },
    /// No security indication in 3 s.
    KeyFailed {
        pairwise: bool,
    },
    MgmtTxDone {
        ok: bool,
    },
    /// A data frame left: `id` is what [`Driver::tx_data`] returned; `acked` is false for no-ack and discard.
    TxFree {
        id: u16,
        acked: bool,
    },
    DataRx(RxFrame),
    /// `WMI_ROAM_REASON_BEACON_MISS`. Linux lets mac80211 probe the AP, then gives up after 3 s without a beacon.
    BeaconLoss,
    Disconnected(Disconnect),
    FwError(Fault),
}

/// Why a command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverError {
    /// Not up yet, or dead.
    NotReady,
    /// Queue, slot or ring full, or a scan or join running: try later.
    Busy,
    /// Argument out of range.
    Invalid,
    /// The call needs a state this driver is not in (no join, for example).
    WrongState,
    NoMem,
    /// Neither `WMI_SERVICE_MGMT_TX_WMI` nor HTT 3: `ath10k_htt_mgmt_tx` was not ported, as this chip does not need it.
    Unsupported,
    Ce(CeError),
    Wmi(wmi::WmiError),
}

/// A scan. Empty `channels` means the whole list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanParams {
    /// Empty: passive (`WMI_SCAN_FLAG_PASSIVE`). An empty SSID asks for all.
    pub ssids: Vec<Vec<u8>>,
    pub channels: Vec<u16>,
    /// IEs for the probe requests (rates, HT, VHT).
    pub ie: Vec<u8>,
}

/// The BSS to join (`ath10k_vdev_start_restart` and `bss_info_changed`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JoinParams {
    pub bssid: [u8; 6],
    pub freq: u32,
    /// The centre of the channel the BSS uses; for 20 MHz `freq`.
    pub center_freq1: u32,
    /// `chan_to_phymode`, see [`wmi::phy_mode`].
    pub phymode: u8,
    /// `chandef->chan->max_power`, dBm.
    pub max_power_dbm: u8,
    pub beacon_interval: u32,
    pub dtim_period: u32,
    pub short_slot: bool,
    pub short_preamble: bool,
}

/// Which cipher a key is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    /// 16 bytes.
    Ccmp,
    /// 32 bytes: the key and the two MIC keys, tx first.
    Tkip,
}

/// A key for the AP's peer (`ath10k_set_key` with the cipher table of `ath10k_send_key`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySpec {
    pub pairwise: bool,
    pub key_idx: u8,
    pub cipher: Cipher,
    pub key: Vec<u8>,
}

/// Counters; nothing depends on them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// WMI events and HTT messages that did not parse.
    pub bad_msg: u32,
    /// HTC frames dropped (`RxDrop`).
    pub htc_drop: u32,
    /// Completions for ids not pending, and the like.
    pub stale_tx: u32,
    /// Management frames dropped for their status or channel.
    pub mgmt_drop: u32,
    pub rx: RxDrops,
    pub rx_delivered: u32,
    /// In-order rx indications, about one per aggregate: `rx_delivered` over this is how much the air aggregates towards us.
    pub rx_in_ord_ind: u32,
    /// `HTT_T2H_MSG_TYPE_RX_ADDBA` and `_RX_DELBA`: the firmware says a block ack session opened or closed.
    pub rx_addba: u32,
    pub rx_delba: u32,
    /// Tx completion messages, the MSDUs in them, and those the peer did not acknowledge.
    pub tx_compl_msgs: u32,
    pub tx_compl_msdus: u32,
    pub tx_not_acked: u32,
}

/// `ath10k_htt_tx_mgmt`-independent bookkeeping of a WMI management frame.
struct MgmtSlot {
    desc_id: u32,
    buf: DmaBuf,
}

/// mac80211's default `dynamic_ps_timeout`, in ms: the `WMI_STA_PS_PARAM_INACTIVITY_TIME` that `ath10k_mac_vif_setup_ps` sends before
/// enabling power save.
pub const DYNAMIC_PS_TIMEOUT_MS: u32 = 100;

struct QueuedCmd {
    frame: Vec<u8>,
    done: Option<Event>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boot {
    Off,
    HtcReady,
    /// `ath10k_htt_connect` outstanding, then `ath10k_wmi_connect`.
    HtcConnect,
    ServiceReady,
    WmiReady,
    HttVersion,
    /// Waiting for the last bring-up command to leave.
    Settle,
    Up,
    Dead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scan {
    Idle,
    Starting(u64),
    Running(u64),
    Aborting(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Idle,
    WaitVdevStart(u64),
    WaitPeerMap(u64),
    Joined,
    /// `ath10k_peer_delete`: unmap or delete response.
    WaitPeerGone(u64),
    /// `ath10k_vdev_stop`.
    WaitVdevStopped(u64),
}

/// `ATH10K_SNOC_RX_POST_RETRY_MS` (`snoc.c`): a receive pipe that could not get a buffer is posted again after this.
const RX_POST_RETRY_MS: u64 = 50;

/// The earlier of two deadlines, `None` being none.
fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// A WCN3990 as a station.
pub struct Driver<P: Platform> {
    plat: P,
    cfg: Config,
    hif: Hif,
    htc: Htc,
    rx: Option<HttRx>,
    tx: Option<HttTx>,
    boot: Boot,
    deadline: Option<u64>,
    now: u64,
    /// When the HTT ring or a CE receive pipe is to be refilled again (`HTT_RX_RING_REFILL_*_MS`,
    /// `ATH10K_SNOC_RX_POST_RETRY_MS`), if it is short.
    refill_due: Option<u64>,
    htt_eid: u8,
    wmi_eid: u8,
    events: Vec<Event>,
    stats: Stats,
    // service state
    svc_main: Vec<u8>,
    svc_ext: Vec<u8>,
    caps: Caps,
    low_5g: u32,
    high_5g: u32,
    htt_major: u8,
    htt_minor: u8,
    mac: [u8; 6],
    mem_chunks: Vec<(wmi::MemChunk, DmaBuf)>,
    /// Service ready seen; `init` goes at the end of the poll, after a service available in the same batch.
    pending_svc_ready: Option<Vec<wmi::MemReq>>,
    // wmi
    wmiq: VecDeque<QueuedCmd>,
    wmi_head_since: Option<u64>,
    // station
    channels: Vec<wmi::ChannelArg>,
    scan: Scan,
    link: Link,
    is_up: bool,
    join: JoinParams,
    /// `arvif->beacon_interval` and `arvif->dtim_period`: 0 until mac80211 reports them, so the first
    /// `vdev_start` carries 0 for both, as in `ath10k_vdev_start_restart`.
    bcn_intval: u32,
    dtim_period: u32,
    /// `arvif->wmm_params`, BE, BK, VI, VO: what each `vdev_wmm_conf` sends whole, one AC updated per `conf_tx`.
    wmm_params: [wmi::WmmAc; 4],
    /// mac80211's defaults after a disassociation, held until `vdev_down` has gone, where `ieee80211_set_disassoc`
    /// puts them.
    wmm_after_down: Option<[wmi::WmmAc; 4]>,
    oper_freq: Option<u32>,
    peer_id: Option<u16>,
    vht_cap: u32,
    keys: VecDeque<KeySpec>,
    key_wait: Option<(bool, u64)>,
    mgmt: Vec<MgmtSlot>,
    next_desc_id: u32,
    win: PnWindows,
    ppdu: PpduStatus,
}

fn get_nss_from_chainmask(mask: u32) -> u32 {
    if mask & 0xf == 0xf {
        4
    } else if mask & 0x7 == 0x7 {
        3
    } else if mask & 0x3 == 0x3 {
        2
    } else {
        1
    }
}

impl<P: Platform> Driver<P> {
    /// Check the configuration. Nothing is touched before [`Driver::start`].
    pub fn new(plat: P, cfg: Config) -> Result<Driver<P>, DriverError> {
        cfg.check()?;
        let htc = Htc::new().map_err(|_| DriverError::Invalid)?;
        Ok(Driver {
            plat,
            cfg,
            hif: Hif::new(&cfg),
            htc,
            rx: None,
            tx: None,
            boot: Boot::Off,
            deadline: None,
            now: 0,
            refill_due: None,
            htt_eid: 0,
            wmi_eid: 0,
            events: Vec::new(),
            stats: Stats::default(),
            svc_main: Vec::new(),
            svc_ext: Vec::new(),
            caps: Caps::default(),
            low_5g: 0,
            high_5g: 0,
            htt_major: 0,
            htt_minor: 0,
            mac: cfg.mac.unwrap_or([0; 6]),
            mem_chunks: Vec::new(),
            pending_svc_ready: None,
            wmiq: VecDeque::new(),
            wmi_head_since: None,
            channels: Vec::new(),
            scan: Scan::Idle,
            link: Link::Idle,
            is_up: false,
            join: JoinParams::default(),
            bcn_intval: 0,
            dtim_period: 0,
            wmm_params: [wmi::WmmAc::default(); 4],
            wmm_after_down: None,
            oper_freq: None,
            peer_id: None,
            vht_cap: 0,
            keys: VecDeque::new(),
            key_wait: None,
            mgmt: Vec::new(),
            next_desc_id: 0,
            win: PnWindows::default(),
            ppdu: PpduStatus::default(),
        })
    }

    /// The arena the engines need with this configuration.
    pub fn arena_bytes(cfg: &Config) -> usize {
        hif::arena_bytes(&hif::host_attrs(cfg))
    }

    /// The platform back, after [`Driver::stop`].
    pub fn into_platform(self) -> P {
        self.plat
    }

    pub fn platform(&self) -> &P {
        &self.plat
    }
    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.plat
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// `htt.target_version_major`, `_minor`.
    pub fn htt_version(&self) -> (u8, u8) {
        (self.htt_major, self.htt_minor)
    }
    /// The channel list given to the firmware, after [`Event::Ready`].
    pub fn channels(&self) -> &[wmi::ChannelArg] {
        &self.channels
    }

    /// `ath10k_snoc_hif_power_up` from `ath10k_ce_alloc_rri`, then the first half of `ath10k_core_start`: HTC init,
    /// HTT tx and rx memory, `ath10k_snoc_hif_start`. The target must already be in mission mode over QMI.
    pub fn start(&mut self) -> Result<(), DriverError> {
        if self.boot != Boot::Off {
            return Err(DriverError::WrongState);
        }
        self.hif.power_up(&mut self.plat).map_err(DriverError::Ce)?;
        {
            let (_, _, dma) = self.plat.parts();
            self.tx = Some(HttTx::alloc(dma, &self.cfg).ok_or(DriverError::NoMem)?);
            self.rx = Some(HttRx::alloc(dma, &self.cfg).ok_or(DriverError::NoMem)?);
        }
        self.hif.post_all(&mut self.plat);
        self.boot = Boot::HtcReady;
        self.deadline = None;
        Ok(())
    }

    /// `ath10k_snoc_hif_stop` and `_power_down`, for after the kernel has stopped the target. Every buffer goes back
    /// to the platform. To start again, make a new driver from [`Driver::into_platform`]: the HTC state, like the
    /// firmware's, starts over.
    pub fn stop(&mut self) {
        self.hif.power_down(&mut self.plat);
        let (_, _, dma) = self.plat.parts();
        if let Some(mut r) = self.rx.take() {
            r.free(dma);
        }
        if let Some(mut t) = self.tx.take() {
            t.free(dma);
        }
        for s in self.mgmt.drain(..) {
            dma.free(s.buf);
        }
        for (_, b) in self.mem_chunks.drain(..) {
            dma.free(b);
        }
        self.wmiq.clear();
        self.boot = Boot::Off;
    }

    /// Every engine's registers and rings, for a log when the firmware does not answer. Reads only.
    pub fn ce_report(&mut self) -> Vec<alloc::string::String> {
        self.hif.report(&mut self.plat)
    }

    // ---- the poll ------------------------------------------------------------------------------------------------

    /// Do everything that can be done without waiting and report it. Call every millisecond or two with a clock that
    /// never goes back.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Event> {
        self.now = now_ms;
        if self.boot == Boot::Off {
            return Vec::new();
        }
        // the data path keeps moving in every state but Off: buffers must keep coming back
        let msgs = self.hif.service(&mut self.plat);
        for m in msgs {
            self.on_message(m);
        }
        if self.boot != Boot::Dead {
            self.rx_work();
        }
        self.refill_due = None;
        if let Some(r) = self.rx.as_mut() {
            let (_, mem, dma) = self.plat.parts();
            // ath10k_htt_rx_ring_refill is after the ready event; before it the ring stays empty
            if !r.confused && matches!(self.boot, Boot::HttVersion | Boot::Settle | Boot::Up) {
                self.refill_due = r.replenish(mem, dma).map(|ms| now_ms.saturating_add(ms));
            }
        }
        if self.hif.rx_owed() {
            // ath10k_snoc_rx_post_pipe failed: its rx_post_retry timer, ATH10K_SNOC_RX_POST_RETRY_MS
            self.refill_due = earliest(self.refill_due, Some(now_ms.saturating_add(RX_POST_RETRY_MS)));
        }
        self.pump_boot();
        self.pump_timeouts();
        self.pump_keys();
        self.pump_wmi();
        core::mem::take(&mut self.events)
    }

    /// `ath10k_snoc_napi_poll`'s first half for the engines whose interrupt came (bit `id` of `mask`): their `HOST_IS` is cleared
    /// before the caller unmasks their lines; the next [`Driver::poll`] drains them.
    pub fn ack_interrupts(&mut self, mask: u32) {
        if self.boot == Boot::Off {
            return;
        }
        self.hif.ack(&mut self.plat, mask);
    }

    /// When [`Driver::poll`] must run again by itself, with nothing from the device in between: the bring-up's wait, a scan's,
    /// a join's, a key's, the WMI head's (`timeout::CMD`, the C's "wmi command timeout"), and a refill's retry. The pumps test
    /// `now > deadline`, so each is due a millisecond after it. `None`: nothing is due; only the device moves the driver.
    pub fn next_deadline(&self) -> Option<u64> {
        let after = |d: u64| Some(d.saturating_add(1));
        let mut due = self.refill_due;
        if matches!(
            self.boot,
            Boot::HtcReady | Boot::HtcConnect | Boot::ServiceReady | Boot::WmiReady | Boot::HttVersion | Boot::Settle
        ) {
            due = earliest(due, self.deadline.and_then(after));
        }
        if let Scan::Starting(d) | Scan::Running(d) | Scan::Aborting(d) = self.scan {
            due = earliest(due, after(d));
        }
        if let Link::WaitVdevStart(d) | Link::WaitPeerMap(d) | Link::WaitPeerGone(d) | Link::WaitVdevStopped(d) = self.link {
            due = earliest(due, after(d));
        }
        if let Some((_, d)) = self.key_wait {
            due = earliest(due, after(d));
        }
        if !self.wmiq.is_empty()
            && let Some(since) = self.wmi_head_since
        {
            due = earliest(due, after(since.saturating_add(timeout::CMD)));
        }
        due
    }

    /// Whether the last [`Driver::poll`] left work the next one does at once, with nothing from the device: the service-ready
    /// work it deferred, or the step from `Settle` to `Up` once the last bring-up command has left.
    pub fn has_work(&self) -> bool {
        self.pending_svc_ready.is_some() || (self.boot == Boot::Settle && self.wmiq.is_empty())
    }

    /// `ath10k_snoc_htt_htc_rx_cb`, `_htc_rx_cb` (HTC) and `_htt_rx_cb` (CE5: `ath10k_snoc_htt_rx_deliver`, no HTC
    /// parsing) and `_pktlog_rx_cb` (trace only).
    fn on_message(&mut self, m: hif::RxMsg) {
        match m.ce {
            5 => {
                if let Some(b) = m.bytes.get(htc::HDR_LEN..) {
                    self.on_htt(b);
                }
            }
            11 => {}
            _ => self.on_htc(&m.bytes),
        }
    }

    fn on_htc(&mut self, frame: &[u8]) {
        let (eid, payload) = match self.htc.rx_frame(frame) {
            Ok(f) => (f.eid, f.payload),
            Err(_) => {
                // credits in a trailer have been applied whatever the verdict
                self.stats.htc_drop += 1;
                return;
            }
        };
        if eid == 0 {
            self.on_htc_control(payload);
        } else if self.wmi_eid != 0 && eid == self.wmi_eid {
            self.on_wmi(payload);
        } else if self.htt_eid != 0 && eid == self.htt_eid {
            self.on_htt(payload);
        } else {
            self.stats.htc_drop += 1;
        }
    }

    /// `ath10k_htc_control_rx_complete`, and what `ath10k_core_start` does between the waits.
    fn on_htc_control(&mut self, payload: &[u8]) {
        match self.htc.on_control_message(payload) {
            Ok(htc::Event::Ready(_)) => {
                self.boot = Boot::HtcConnect;
                self.set_deadline(timeout::HTC);
                self.send_connect(htc_svc::HTT_DATA_MSG);
            }
            Ok(htc::Event::Connected(c)) => {
                if c.service_id == htc_svc::HTT_DATA_MSG {
                    self.htt_eid = c.eid;
                    self.set_deadline(timeout::HTC);
                    self.send_connect(htc_svc::WMI_CONTROL);
                } else if c.service_id == htc_svc::WMI_CONTROL {
                    self.wmi_eid = c.eid;
                    match self.htc.start() {
                        Ok(pkt) => {
                            self.send_htc_pkt(&pkt);
                            self.boot = Boot::ServiceReady;
                            self.set_deadline(timeout::WMI);
                        }
                        Err(_) => self.fail(Fault::Htc),
                    }
                }
            }
            Ok(_) => {}
            Err(_) => self.fail(Fault::Htc),
        }
    }

    fn send_connect(&mut self, service: u16) {
        match self.htc.connect_service(service) {
            Ok(pkt) => self.send_htc_pkt(&pkt),
            Err(_) => self.fail(Fault::Htc),
        }
    }

    /// `ath10k_htc_send` on endpoint 0 and the HTT endpoint, which have no credit flow control.
    fn send_htc_pkt(&mut self, pkt: &htc::OutPacket) {
        let eid = htc::Hdr::parse(&pkt.bytes).map_or(0, |h| h.eid);
        if let Err(e) = self.hif.send(&mut self.plat, pkt.ul_pipe, eid, &pkt.bytes) {
            self.fail(Fault::Ce(e));
        }
    }

    fn send_htt(&mut self, msg: &[u8]) {
        match self.htc.frame_for(self.htt_eid, msg) {
            Ok(pkt) => self.send_htc_pkt(&pkt),
            Err(_) => self.fail(Fault::Htc),
        }
    }

    fn fail(&mut self, f: Fault) {
        if self.boot != Boot::Dead {
            self.boot = Boot::Dead;
            self.events.push(Event::FwError(f));
        }
    }

    fn set_deadline(&mut self, ms: u64) {
        self.deadline = Some(self.now.saturating_add(ms));
    }

    // ---- WMI events ----------------------------------------------------------------------------------------------

    /// `ath10k_wmi_tlv_op_rx`.
    fn on_wmi(&mut self, buf: &[u8]) {
        let Ok((id, tlvs)) = wmi::rx(buf) else {
            self.stats.bad_msg += 1;
            return;
        };
        let ok = match id {
            EventId::ServiceReady => self.ev_service_ready(tlvs),
            EventId::Ready => self.ev_ready(tlvs),
            EventId::ServiceAvailable => self.ev_service_available(tlvs),
            EventId::Scan => self.ev_scan(tlvs),
            EventId::MgmtRx => self.ev_mgmt_rx(tlvs),
            EventId::MgmtTxCompletion => match wmi::parse_mgmt_tx_compl(tlvs) {
                Ok(c) => {
                    self.mgmt_done(c);
                    true
                }
                Err(_) => false,
            },
            EventId::MgmtTxBundleCompletion => match wmi::parse_mgmt_tx_bundle_compl(tlvs) {
                Ok(v) => {
                    for c in v {
                        self.mgmt_done(c);
                    }
                    true
                }
                Err(_) => false,
            },
            EventId::VdevStartResp => self.ev_vdev_start(tlvs),
            EventId::VdevStopped => {
                self.ev_vdev_stopped();
                true
            }
            EventId::PeerStaKickout => match wmi::parse_peer_kickout(tlvs) {
                Ok(addr) => {
                    if self.link == Link::Joined && addr == self.join.bssid {
                        self.events.push(Event::Disconnected(Disconnect::Kickout));
                    }
                    true
                }
                Err(_) => false,
            },
            EventId::PeerDeleteResp => {
                if let Link::WaitPeerGone(_) = self.link {
                    self.peer_gone();
                }
                true
            }
            EventId::Roam => match wmi::parse_roam(tlvs) {
                Ok(r) => {
                    if r.reason == wmi::ROAM_REASON_BEACON_MISS
                        && r.vdev_id == VDEV_ID
                        && self.link == Link::Joined
                    {
                        self.events.push(Event::BeaconLoss);
                    }
                    true
                }
                Err(_) => false,
            },
            // debugging or logged only in Linux, or (tx pause) multi-channel and P2P only
            _ => true,
        };
        if !ok {
            self.stats.bad_msg += 1;
        }
    }

    /// `ath10k_wmi_event_service_ready_work`. The memory and the init command wait for the end of the poll.
    fn ev_service_ready(&mut self, tlvs: &[u8]) -> bool {
        if self.boot != Boot::ServiceReady {
            return true;
        }
        match wmi::parse_service_ready(tlvs) {
            Ok(s) => {
                self.svc_main = s.service_map.to_vec();
                // "hardware advertises support for more spatial streams than it should" limits to max_spatial_stream
                let chains = s.num_rf_chains.clamp(1, MAX_SPATIAL_STREAM);
                self.caps = Caps {
                    ht_cap_info: s.ht_cap,
                    vht_cap_info: s.vht_cap,
                    vht_supp_mcs: s.vht_supp_mcs,
                    num_rf_chains: chains,
                    eeprom_rd: s.eeprom_rd,
                    regdomain: 0,
                };
                self.low_5g = s.low_5ghz_chan;
                self.high_5g = s.high_5ghz_chan;
                self.pending_svc_ready = Some(s.mem_reqs);
                true
            }
            Err(e) => {
                self.fail(Fault::ServiceReady(e));
                false
            }
        }
    }

    fn ev_service_available(&mut self, tlvs: &[u8]) -> bool {
        match wmi::parse_service_available(tlvs) {
            Ok(Some(a)) => {
                self.svc_ext = a.service_map_ext.to_vec();
                true
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// `ath10k_wmi_event_ready` and what `ath10k_core_start` does after the wait: ring refill and `ath10k_htt_setup`'s
    /// version request.
    fn ev_ready(&mut self, tlvs: &[u8]) -> bool {
        let Ok(r) = wmi::parse_ready(tlvs) else {
            return false;
        };
        if self.boot != Boot::WmiReady {
            return true;
        }
        if self.cfg.mac.is_none() {
            self.mac = r.mac_addr;
        }
        let in_ord = self.has_svc(wmi::svc::RX_FULL_REORDER);
        if let Some(rx) = self.rx.as_mut() {
            rx.in_ord = in_ord;
            let (_, mem, dma) = self.plat.parts();
            if !rx.refill(mem, dma) {
                self.fail(Fault::NoMem);
                return true;
            }
        }
        self.boot = Boot::HttVersion;
        self.set_deadline(timeout::HTT);
        self.send_htt(&htt::ver_req());
        true
    }

    /// `WMI_SERVICE_*` through `wmi_tlv_svc_map` and `wmi_tlv_svc_map_ext`.
    fn has_svc(&self, x: u32) -> bool {
        if x < wmi::svc::MAX_SERVICE {
            wmi::svc_is_enabled(&self.svc_main, x, self.svc_main.len() as u32)
        } else {
            wmi::svc_is_enabled(&self.svc_ext, x, wmi::svc::MAX_SERVICE)
        }
    }

    /// `ath10k_wmi_event_scan`.
    fn ev_scan(&mut self, tlvs: &[u8]) -> bool {
        let Ok(e) = wmi::parse_scan_event(tlvs) else {
            return false;
        };
        match e.event_type {
            wmi::scan_event::STARTED => {
                if let Scan::Starting(_) = self.scan {
                    // ath10k_hw_scan's timeout: max_scan_time plus a margin
                    self.scan = Scan::Running(self.now.saturating_add(20_000 + 200));
                }
            }
            wmi::scan_event::COMPLETED => match self.scan {
                // "...may be just firmware's scan state machine recovering": ignored while starting
                Scan::Running(_) => self.scan_finish(false),
                Scan::Aborting(_) => self.scan_finish(true),
                _ => {}
            },
            wmi::scan_event::START_FAILED => {
                if let Scan::Starting(_) = self.scan {
                    self.scan_finish(true);
                }
            }
            _ => {}
        }
        true
    }

    fn scan_finish(&mut self, aborted: bool) {
        self.scan = Scan::Idle;
        self.events.push(Event::ScanDone { aborted });
    }

    /// `ath10k_wmi_event_mgmt_rx`.
    fn ev_mgmt_rx(&mut self, tlvs: &[u8]) -> bool {
        let Ok(m) = wmi::parse_mgmt_rx(tlvs) else {
            return false;
        };
        if m.status
            & (wmi::rx_status::ERR_DECRYPT
                | wmi::rx_status::ERR_KEY_CACHE_MISS
                | wmi::rx_status::ERR_CRC)
            != 0
            || m.frame.len() < 24
        {
            self.stats.mgmt_drop += 1;
            return true;
        }
        let freq = match m.channel {
            14 => 2484,
            1..=13 => 2407 + 5 * m.channel,
            36..=MAX_5G_CHAN => 5000 + 5 * m.channel,
            _ => {
                self.stats.mgmt_drop += 1;
                return true;
            }
        };
        let mut frame = m.frame.to_vec();
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        // ath10k_wmi_rx_is_decrypted, with sw_decrypt_mcast_mgmt set for this chip; the DA of a management frame is
        // addr1. A beacon or an action frame keeps its bit as in Linux, which sets flags for mac80211.
        let stype = (fc >> 4) & 0xf;
        if fc & 0x4000 != 0
            && stype != 0xb
            && frame[4] & 1 == 0
            && !matches!(stype, 0xd | 0xc | 0xa)
        {
            frame[1] &= !0x40;
        }
        self.events.push(Event::MgmtRx {
            frame,
            rssi: NOISE_FLOOR.saturating_add(m.snr as i32),
            freq,
        });
        true
    }

    /// `wmi_process_mgmt_tx_comp`.
    fn mgmt_done(&mut self, c: wmi::MgmtTxCompl) {
        let Some(i) = self.mgmt.iter().position(|s| s.desc_id == c.desc_id) else {
            self.stats.stale_tx += 1;
            return;
        };
        let s = self.mgmt.swap_remove(i);
        let (_, _, dma) = self.plat.parts();
        dma.free(s.buf);
        self.events.push(Event::MgmtTxDone { ok: c.status == 0 });
    }

    /// `ath10k_wmi_event_vdev_start_resp`, then what mac80211 does next: the BSS parameters and the peer.
    fn ev_vdev_start(&mut self, tlvs: &[u8]) -> bool {
        let Ok(r) = wmi::parse_vdev_start_response(tlvs) else {
            return false;
        };
        if let Link::WaitVdevStart(_) = self.link {
            if r.status != 0 || r.vdev_id != VDEV_ID {
                self.link = Link::Idle;
                self.events.push(Event::JoinFailed);
                return true;
            }
            self.oper_freq = Some(self.join.freq);
            let j = self.join;
            let v = VDEV_ID;
            // BSS_CHANGED_BEACON_INT, which mac80211 sends right after the channel context is assigned; DTIM,
            // slot time and preamble come with the association (peer_assoc)
            self.bcn_intval = j.beacon_interval;
            self.push_q(
                cmd::VDEV_SET_PARAM,
                wmi::gen_vdev_set_param(v, wmi::vdev_param::BEACON_INTERVAL, j.beacon_interval),
            );
            self.push_q(
                cmd::STA_POWERSAVE_MODE,
                wmi::gen_set_psmode(v, sta_ps::MODE_DISABLED),
            );
            // ath10k_mac_txpower_setup. Transmit power is left exactly as it was (no change to TX power or
            // regulatory fields, owner's rule; audit D6)
            let p = u32::from(j.max_power_dbm) * 2;
            self.push_q(
                cmd::PDEV_SET_PARAM,
                wmi::gen_pdev_set_param(wmi::pdev_param::TXPOWER_LIMIT2G, p),
            );
            self.push_q(
                cmd::PDEV_SET_PARAM,
                wmi::gen_pdev_set_param(wmi::pdev_param::TXPOWER_LIMIT5G, p),
            );
            self.peer_id = None;
            self.push_q(
                cmd::PEER_CREATE,
                wmi::gen_peer_create(v, &j.bssid, wmi::peer_type::DEFAULT),
            );
            self.link = Link::WaitPeerMap(self.now.saturating_add(timeout::CMD));
        }
        true
    }

    /// `ath10k_wmi_event_vdev_stopped`.
    fn ev_vdev_stopped(&mut self) {
        if let Link::WaitVdevStopped(_) = self.link {
            self.link = Link::Idle;
            self.oper_freq = None;
            self.events.push(Event::Disconnected(Disconnect::Left));
        }
    }

    // ---- HTT messages --------------------------------------------------------------------------------------------

    /// `ath10k_htt_t2h_msg_handler` over `htt_tlv_t2h_msg_types`.
    fn on_htt(&mut self, buf: &[u8]) {
        let Ok((t, _)) = htt::parse_resp(buf) else {
            self.stats.bad_msg += 1;
            return;
        };
        let ok = match t {
            T2h::VersionConf => match htt::parse_version_conf(buf) {
                Ok((major, minor)) => {
                    self.on_version(major, minor);
                    true
                }
                Err(_) => false,
            },
            T2h::RxInd => match htt::parse_rx_ind(buf) {
                Ok(r) => {
                    if let Some(rx) = self.rx.as_mut() {
                        rx.mpdus_ready = rx.mpdus_ready.saturating_add(r.mpdu_count);
                    }
                    true
                }
                Err(_) => false,
            },
            // ath10k_htt_rx_proc_rx_frag_ind has no LL op: counted, popped like any other
            T2h::RxFragInd => {
                if let Some(rx) = self.rx.as_mut() {
                    rx.mpdus_ready = rx.mpdus_ready.saturating_add(1);
                }
                true
            }
            T2h::RxInOrdPaddrInd => match htt::parse_rx_in_ord_ind(buf) {
                Ok(ind) => {
                    self.stats.rx_in_ord_ind = self.stats.rx_in_ord_ind.saturating_add(1);
                    self.rx_in_ord(&ind);
                    true
                }
                Err(_) => false,
            },
            T2h::PeerMap => match htt::parse_peer_map(buf) {
                Ok((vdev, id, addr)) => {
                    self.on_peer_map(vdev, id, addr);
                    true
                }
                Err(_) => false,
            },
            T2h::PeerUnmap => match htt::parse_peer_unmap(buf) {
                Ok(id) => {
                    if self.peer_id == Some(id) {
                        self.peer_id = None;
                        if let Link::WaitPeerGone(_) = self.link {
                            self.peer_gone();
                        }
                    }
                    true
                }
                Err(_) => false,
            },
            T2h::TxComplInd => match htt::parse_tx_completion(buf) {
                Ok(c) => {
                    self.stats.tx_compl_msgs = self.stats.tx_compl_msgs.saturating_add(1);
                    self.stats.tx_compl_msdus =
                        self.stats.tx_compl_msdus.saturating_add(c.msdus.len() as u32);
                    if c.result != TxResult::Ack {
                        self.stats.tx_not_acked =
                            self.stats.tx_not_acked.saturating_add(c.msdus.len() as u32);
                    }
                    for m in &c.msdus {
                        self.tx_done(m.msdu_id, c.result == TxResult::Ack);
                    }
                    true
                }
                Err(_) => false,
            },
            T2h::MgmtTxCompl => match htt::parse_mgmt_tx_compl(buf) {
                Ok((id, status)) => {
                    if let Ok(id) = u16::try_from(id) {
                        self.tx_done(id, status == htt::mgmt_tx_status::OK);
                    }
                    true
                }
                Err(_) => false,
            },
            T2h::SecInd => match htt::parse_sec_ind(buf) {
                Ok(_) => {
                    self.on_key_done();
                    true
                }
                Err(_) => false,
            },
            T2h::RxAddba => {
                self.stats.rx_addba = self.stats.rx_addba.saturating_add(1);
                true
            }
            T2h::RxDelba => {
                self.stats.rx_delba = self.stats.rx_delba.saturating_add(1);
                true
            }
            _ => true,
        };
        if !ok {
            self.stats.bad_msg += 1;
        }
    }

    /// `ath10k_txrx_tx_unref`.
    fn tx_done(&mut self, id: u16, acked: bool) {
        let Some(tx) = self.tx.as_mut() else {
            return;
        };
        match tx.complete(id) {
            Some(TxKind::Data) => self.events.push(Event::TxFree { id, acked }),
            Some(TxKind::Mgmt) => self.events.push(Event::MgmtTxDone { ok: acked }),
            None => self.stats.stale_tx += 1,
        }
    }

    /// `ath10k_htt_verify_version`, the rest of `ath10k_htt_setup`, and `ath10k_start`.
    fn on_version(&mut self, major: u8, minor: u8) {
        if self.boot != Boot::HttVersion {
            return;
        }
        self.htt_major = major;
        self.htt_minor = minor;
        if major != 2 && major != 3 {
            self.fail(Fault::HttVersion(major));
            return;
        }
        // ath10k_htt_send_frag_desc_bank_cfg, _send_rx_ring_cfg, _h2t_aggr_cfg_msg
        if let Some(tx) = self.tx.as_ref() {
            let m = htt::frag_desc_bank_cfg_64(tx.frag_paddr(), tx.max_id());
            self.send_htt(&m);
        }
        if let Some(rx) = self.rx.as_ref() {
            let (_, _, dma) = self.plat.parts();
            let idx = rx.alloc_idx_value(dma) as u16;
            let setup = htt::RxRingSetup {
                fw_idx_shadow_reg_paddr: rx.alloc_idx_paddr(),
                rx_ring_base_paddr: rx.ring_paddr(),
                rx_ring_len: rx.size(),
                rx_ring_bufsize: htt::RX_BUF_SIZE,
                flags: htt::rx_ring_flags::ALL,
                fw_idx_init_val: idx,
                offsets: htt::WCN3990_RX_DESC_OFFSETS,
            };
            let m = htt::rx_ring_setup_64(&setup);
            self.send_htt(&m);
        }
        if let Some(m) = htt::aggr_cfg_v2(htt::MAX_NUM_AMPDU_DEFAULT, htt::MAX_NUM_AMSDU_DEFAULT) {
            self.send_htt(&m);
        }
        self.bringup_commands();
        self.boot = Boot::Settle;
        self.set_deadline(timeout::WMI);
    }

    /// `ath10k_start` after `ath10k_core_start`, then `ath10k_add_interface` for the one station.
    fn bringup_commands(&mut self) {
        use wmi::pdev_param as pp;
        let pdev =
            |d: &mut Self, p, v| d.push_q(cmd::PDEV_SET_PARAM, wmi::gen_pdev_set_param(p, v));
        pdev(self, pp::PMF_QOS, 1);
        pdev(self, pp::DYNAMIC_BW, 1);
        if self.has_svc(wmi::svc::SPOOF_MAC_SUPPORT) {
            let m = self.mac;
            let oui = u32::from(m[0]) << 16 | u32::from(m[1]) << 8 | u32::from(m[2]);
            self.push_q(cmd::SCAN_PROB_REQ_OUI, wmi::gen_scan_prob_req_oui(oui));
        }
        if self.has_svc(wmi::svc::ADAPTIVE_OCS) {
            self.push_q(cmd::RESMGR_ADAPTIVE_OCS, wmi::gen_adaptive_qcs(true));
        }
        if self.has_svc(wmi::svc::BURST) {
            pdev(self, pp::BURST_ENABLE, 0);
        }
        pdev(self, pp::IDLE_PS_CONFIG, 1);
        // __ath10k_set_antenna with the chain masks of service ready
        let mask = (1u32 << self.caps.num_rf_chains) - 1;
        pdev(self, pp::TX_CHAIN_MASK, mask);
        pdev(self, pp::RX_CHAIN_MASK, mask);
        pdev(self, pp::ARP_AC_OVERRIDE, 0);
        pdev(self, pp::ANI_ENABLE, 1);
        // ath10k_regd_update
        self.channels = regd::channels(self.low_5g, self.high_5g);
        if let Ok(p) = wmi::gen_scan_chan_list(&self.channels) {
            self.push_q(cmd::SCAN_CHAN_LIST, p);
        }
        let (rd, ctl5, ctl2) = regd::regpair_for(self.caps.eeprom_rd);
        self.caps.regdomain = rd;
        self.push_q(
            cmd::PDEV_SET_REGDOMAIN,
            wmi::gen_pdev_set_regdomain(rd, rd, rd, ctl2, ctl5),
        );
        // ath10k_add_interface
        let v = VDEV_ID;
        let m = self.mac;
        self.push_q(
            cmd::VDEV_CREATE,
            wmi::gen_vdev_create(v, vdev_type::STA, wmi::vdev_subtype::NONE, &m),
        );
        if self.has_svc(wmi::svc::STA_KEEP_ALIVE) {
            self.push_q(cmd::STA_KEEPALIVE, wmi::gen_sta_keepalive_disable(v));
        }
        self.push_q(
            cmd::VDEV_SET_PARAM,
            wmi::gen_vdev_set_param(v, wmi::vdev_param::TX_ENCAP_TYPE, TXRX_NATIVE_WIFI),
        );
        self.push_q(
            cmd::VDEV_SET_PARAM,
            wmi::gen_vdev_set_param(v, wmi::vdev_param::NSS, get_nss_from_chainmask(mask)),
        );
        for (p, val) in [
            (sta_ps::PARAM_RX_WAKE_POLICY, sta_ps::RX_WAKE_POLICY_WAKE),
            (
                sta_ps::PARAM_TX_WAKE_THRESHOLD,
                sta_ps::TX_WAKE_THRESHOLD_ALWAYS,
            ),
            (sta_ps::PARAM_PSPOLL_COUNT, sta_ps::PSPOLL_COUNT_NO_MAX),
        ] {
            self.push_q(cmd::STA_POWERSAVE_PARAM, wmi::gen_set_sta_ps(v, p, val));
        }
        // ath10k_mac_set_rts with wiphy's default (u32)-1
        self.push_q(
            cmd::VDEV_SET_PARAM,
            wmi::gen_vdev_set_param(v, wmi::vdev_param::RTS_THRESHOLD, u32::MAX),
        );
        // ieee80211_do_open after drv_add_interface: ieee80211_reset_erp_info flags slot and preamble as changed, so
        // ath10k_bss_info_changed sends the long values
        self.erp(false, false, true);
    }

    fn on_peer_map(&mut self, vdev: u8, id: u16, addr: [u8; 6]) {
        // ath10k_peer_map_event's bound on the ids is the table it indexes; here the id is only compared
        if u32::from(vdev) != VDEV_ID {
            return;
        }
        if let Link::WaitPeerMap(_) = self.link
            && addr == self.join.bssid
        {
            self.peer_id = Some(id);
            self.link = Link::Joined;
            self.events.push(Event::VdevStarted);
        }
    }

    /// `ath10k_peer_delete` is done: on to `ath10k_bss_disassoc`'s `vdev_down` and `ath10k_vdev_stop`.
    fn peer_gone(&mut self) {
        self.peer_id = None;
        if self.is_up {
            self.is_up = false;
            // ieee80211_set_disassoc: ieee80211_reset_erp_info's slot and preamble go with BSS_CHANGED_ASSOC and are
            // handled before ath10k_bss_disassoc's vdev_down
            self.erp(false, false, true);
            self.push_q(cmd::VDEV_DOWN, wmi::gen_vdev_down(VDEV_ID));
        }
        if let Some(acs) = self.wmm_after_down.take() {
            self.conf_tx_all(&acs);
        }
        self.recalc_txbf(0);
        self.push_q(cmd::VDEV_STOP, wmi::gen_vdev_stop(VDEV_ID));
        self.link = Link::WaitVdevStopped(self.now.saturating_add(timeout::VDEV));
    }

    fn on_key_done(&mut self) {
        let Some((pairwise, _)) = self.key_wait.take() else {
            return;
        };
        self.win.reset(pairwise);
        if pairwise {
            // ath10k_set_key: "sta && cmd == SET_KEY && PAIRWISE"
            let bssid = self.join.bssid;
            self.push_q(
                cmd::PEER_SET_PARAM,
                wmi::gen_peer_set_param(VDEV_ID, &bssid, wmi::peer_param::AUTHORIZE, 1),
            );
            if self.cfg.power_save {
                self.push_q(
                    cmd::STA_POWERSAVE_PARAM,
                    wmi::gen_set_sta_ps(VDEV_ID, sta_ps::PARAM_INACTIVITY_TIME, DYNAMIC_PS_TIMEOUT_MS),
                );
                self.push_q(
                    cmd::STA_POWERSAVE_MODE,
                    wmi::gen_set_psmode(VDEV_ID, sta_ps::MODE_ENABLED),
                );
            }
        }
        self.events.push(Event::KeyInstalled { pairwise });
    }

    // ---- rx -------------------------------------------------------------------------------------------------------

    /// `ath10k_htt_txrx_compl_task`: the MPDUs that rx indications announced.
    fn rx_work(&mut self) {
        let mut frames = Vec::new();
        let freq = self.oper_freq;
        let (_, _, dma) = self.plat.parts();
        let Some(rx) = self.rx.as_mut() else {
            return;
        };
        while rx.mpdus_ready > 0 && !rx.confused {
            rx.mpdus_ready -= 1;
            match rx.amsdu_pop(dma) {
                Ok((msdus, chained)) => {
                    let ctx = RxCtx {
                        freq,
                        peer_id: self.peer_id.unwrap_or(htt::INVALID_PEERID),
                        check_pn: true,
                    };
                    data::classify(
                        msdus,
                        chained,
                        &ctx,
                        &mut self.ppdu,
                        &mut self.win,
                        &mut self.stats.rx,
                        &mut frames,
                    );
                }
                Err(()) => {
                    rx.confused = true;
                    rx.mpdus_ready = 0;
                }
            }
        }
        let confused = rx.confused;
        for f in frames {
            self.stats.rx_delivered += 1;
            self.events.push(Event::DataRx(f));
        }
        if confused {
            self.fail(Fault::RxConfused);
        }
    }

    /// `ath10k_htt_rx_in_ord_ind`.
    fn rx_in_ord(&mut self, ind: &htt::InOrdInd) {
        let mut frames = Vec::new();
        let freq = self.oper_freq;
        let (_, _, dma) = self.plat.parts();
        let Some(rx) = self.rx.as_mut() else {
            return;
        };
        if rx.confused {
            return;
        }
        if ind.offload {
            // ath10k_htt_rx_h_rx_offload: "already decrypted but firmware insists they are protected"
            match rx.pop_offload(dma, ind) {
                Ok(list) => {
                    for mut frame in list {
                        if frame.len() >= 2 {
                            frame[1] &= !0x40;
                        }
                        let off = frame.get(..2).map_or(0, |b| {
                            data::hdrlen(u16::from_le_bytes([b[0], b[1]])).div_ceil(4) * 4
                        });
                        if let Some(freq) = freq {
                            frames.push(RxFrame {
                                frame,
                                decap: Decap::NativeWifi,
                                payload_off: off,
                                air_hdr: [0; 64],
                                decrypted: true,
                                mcast: false,
                                tid: ind.tid,
                                peer_id: ind.peer_id,
                                freq,
                                rssi: NOISE_FLOOR,
                                rate: RxRate::Unknown,
                                pn: 0,
                            });
                        } else {
                            self.stats.rx.no_channel += 1;
                        }
                    }
                }
                Err(()) => rx.confused = true,
            }
        } else {
            match rx.pop_in_ord(dma, ind) {
                Ok(mpdus) => {
                    let ctx = RxCtx {
                        freq,
                        peer_id: ind.peer_id,
                        check_pn: false,
                    };
                    for msdus in mpdus {
                        data::classify(
                            msdus,
                            false,
                            &ctx,
                            &mut self.ppdu,
                            &mut self.win,
                            &mut self.stats.rx,
                            &mut frames,
                        );
                    }
                }
                Err(()) => rx.confused = true,
            }
        }
        let confused = rx.confused;
        for f in frames {
            self.stats.rx_delivered += 1;
            self.events.push(Event::DataRx(f));
        }
        if confused {
            self.fail(Fault::RxConfused);
        }
    }

    // ---- pumps ----------------------------------------------------------------------------------------------------

    fn pump_boot(&mut self) {
        if let Some(reqs) = self.pending_svc_ready.take()
            && self.boot == Boot::ServiceReady
        {
            self.finish_service_ready(&reqs);
        }
        match self.boot {
            Boot::Settle if self.wmiq.is_empty() => {
                self.boot = Boot::Up;
                self.deadline = None;
                self.events.push(Event::Ready {
                    mac: self.mac,
                    caps: self.caps,
                });
            }
            Boot::HtcReady
            | Boot::HtcConnect
            | Boot::ServiceReady
            | Boot::WmiReady
            | Boot::HttVersion
            | Boot::Settle => {
                if self.deadline.is_none() {
                    // ath10k_htc_wait_target waits twice, polling the engines in between
                    let wait = if self.boot == Boot::HtcReady {
                        self.cfg.htc_ready_ms
                    } else {
                        2 * timeout::HTC
                    };
                    self.set_deadline(wait);
                }
                if self.deadline.is_some_and(|d| self.now > d) {
                    let stage = match self.boot {
                        Boot::HtcReady => BootStage::HtcReady,
                        Boot::HtcConnect => BootStage::HtcConnect,
                        Boot::ServiceReady => BootStage::ServiceReady,
                        Boot::WmiReady => BootStage::WmiReady,
                        Boot::HttVersion => BootStage::HttVersion,
                        _ => BootStage::Settle,
                    };
                    self.fail(Fault::BootTimeout(stage));
                }
            }
            _ => {}
        }
    }

    /// The second half of `ath10k_wmi_event_service_ready_work` and `ath10k_wmi_cmd_init`.
    fn finish_service_ready(&mut self, reqs: &[wmi::MemReq]) {
        let (_, _, dma) = self.plat.parts();
        for r in reqs {
            // ath10k_wmi_event_service_ready_work's NUM_UNITS_IS_* (no active peer count on this firmware)
            let units = if r.num_unit_info & 0x6 != 0 {
                PEERS_UNITS
            } else if r.num_unit_info & 0x1 != 0 {
                VDEVS_UNITS
            } else {
                r.num_units
            };
            let size = u64::from(r.unit_size.div_ceil(4)) * 4 * u64::from(units);
            if size == 0 {
                continue;
            }
            if size > MAX_MEM_CHUNK as u64 {
                self.fail(Fault::NoMem);
                return;
            }
            // dma_alloc_coherent in the C: the firmware writes these chunks itself
            let Some(buf) = dma.alloc_coherent(size as usize) else {
                self.fail(Fault::NoMem);
                return;
            };
            let chunk = wmi::MemChunk {
                req_id: r.req_id,
                len: size as u32,
                paddr: buf.paddr,
            };
            self.mem_chunks.push((chunk, buf));
        }
        let chunks: Vec<wmi::MemChunk> = self.mem_chunks.iter().map(|c| c.0).collect();
        let p = wmi::InitParams {
            chunks: &chunks,
            rx_decap_mode: TXRX_NATIVE_WIFI,
            max_num_pending_tx: wmi::wcn3990::NUM_MSDU_DESC,
            max_num_wow_patterns: NUM_WOW_PATTERNS,
            svc_rx_full_reorder: self.has_svc(wmi::svc::RX_FULL_REORDER),
            svc_tx_data_ack_rssi: self.has_svc(wmi::svc::TX_DATA_MGMT_ACK_RSSI),
            svc_support_extend_address: self.has_svc(wmi::svc::SUPPORT_EXTEND_ADDRESS),
        };
        match wmi::gen_init(&p) {
            Ok(body) => {
                self.push_q(cmd::INIT, body);
                self.boot = Boot::WmiReady;
                self.set_deadline(timeout::WMI);
            }
            Err(e) => self.fail(Fault::ServiceReady(e)),
        }
    }

    fn pump_timeouts(&mut self) {
        let now = self.now;
        match self.scan {
            Scan::Starting(d) if now > d => {
                // ath10k_start_scan: stop it, and report failure when that finishes or times out
                self.push_stop_scan();
                self.scan = Scan::Aborting(now.saturating_add(timeout::SCAN_STOP));
            }
            Scan::Running(d) if now > d => {
                // ath10k_scan_timeout_work
                self.push_stop_scan();
                self.scan = Scan::Aborting(now.saturating_add(timeout::SCAN_STOP));
            }
            Scan::Aborting(d) if now > d => self.scan_finish(true),
            _ => {}
        }
        match self.link {
            Link::WaitVdevStart(d) if now > d => {
                self.link = Link::Idle;
                self.events.push(Event::JoinFailed);
            }
            // The vdev is running by now. A failed `ath10k_peer_create` makes mac80211 unassign the channel context,
            // which is `ath10k_vdev_stop`; the next join may not start it a second time. The firmware's answer ends in
            // the `Left` of `ev_vdev_stopped`.
            Link::WaitPeerMap(d) if now > d => {
                self.events.push(Event::JoinFailed);
                self.push_q(cmd::VDEV_STOP, wmi::gen_vdev_stop(VDEV_ID));
                self.link = Link::WaitVdevStopped(now.saturating_add(timeout::VDEV));
            }
            Link::WaitPeerGone(d) if now > d => self.peer_gone(),
            Link::WaitVdevStopped(d) if now > d => self.ev_vdev_stopped(),
            _ => {}
        }
        if let Some((pairwise, d)) = self.key_wait
            && now > d
        {
            self.key_wait = None;
            self.events.push(Event::KeyFailed { pairwise });
        }
    }

    /// Keys go one at a time: `ath10k_install_key` waits for the security indication of each.
    fn pump_keys(&mut self) {
        if self.key_wait.is_some() {
            return;
        }
        let Some(k) = self.keys.pop_front() else {
            return;
        };
        let (cipher, mic) = match k.cipher {
            Cipher::Ccmp => (wmi::cipher::AES_CCM, 0),
            Cipher::Tkip => (wmi::cipher::TKIP, 8),
        };
        let arg = wmi::InstallKeyArg {
            vdev_id: VDEV_ID,
            macaddr: self.join.bssid,
            key_idx: u32::from(k.key_idx),
            key_flags: if k.pairwise {
                wmi::key_flags::PAIRWISE
            } else {
                wmi::key_flags::GROUP
            },
            key_cipher: cipher,
            key_len: k.key.len() as u32,
            key_txmic_len: mic,
            key_rxmic_len: mic,
            data: Some(&k.key),
        };
        match wmi::gen_vdev_install_key(&arg) {
            Ok(body) => {
                self.push_q(cmd::VDEV_INSTALL_KEY, body);
                self.key_wait = Some((k.pairwise, self.now.saturating_add(timeout::CMD)));
            }
            Err(_) => self.events.push(Event::KeyFailed {
                pairwise: k.pairwise,
            }),
        }
    }

    fn push_q(&mut self, id: u32, body: Vec<u8>) {
        self.wmiq.push_back(QueuedCmd {
            frame: wmi::frame(id, &body),
            done: None,
        });
    }

    /// `ath10k_wmi_cmd_send`, without the sleep: what has credits goes, in order. A head that waits 3 s is the C's
    /// "wmi command timeout, restarting hardware".
    fn pump_wmi(&mut self) {
        if self.wmi_eid == 0 {
            return;
        }
        while let Some(front) = self.wmiq.front() {
            let pkt = match self.htc.frame_for(self.wmi_eid, &front.frame) {
                Ok(p) => p,
                Err(htc::HtcError::NoCredits) => {
                    if self.boot == Boot::Dead {
                        return;
                    }
                    let since = *self.wmi_head_since.get_or_insert(self.now);
                    if self.now.saturating_sub(since) > timeout::CMD {
                        self.fail(Fault::WmiStall);
                    }
                    return;
                }
                Err(_) => {
                    // too long for HTC, which an encoder here cannot produce; the command is lost, not the driver
                    self.stats.bad_msg += 1;
                    self.wmiq.pop_front();
                    continue;
                }
            };
            let eid = self.wmi_eid;
            let total = pkt.bytes.len() as u32;
            match self.hif.send(&mut self.plat, pkt.ul_pipe, eid, &pkt.bytes) {
                Ok(()) => {
                    self.wmi_head_since = None;
                    if let Some(c) = self.wmiq.pop_front()
                        && let Some(ev) = c.done
                    {
                        self.events.push(ev);
                    }
                }
                Err(_) => {
                    // ring full or no memory: the credit goes back, the next poll tries again
                    self.htc.release_credit(eid, total);
                    let since = *self.wmi_head_since.get_or_insert(self.now);
                    if self.now.saturating_sub(since) > timeout::CMD {
                        self.fail(Fault::WmiStall);
                    }
                    return;
                }
            }
        }
    }

    // ---- commands -------------------------------------------------------------------------------------------------

    fn is_up(&self) -> Result<(), DriverError> {
        if self.boot == Boot::Up {
            Ok(())
        } else {
            Err(DriverError::NotReady)
        }
    }

    /// `ath10k_hw_scan` with `ath10k_wmi_start_scan_init`'s values.
    pub fn start_scan(&mut self, p: &ScanParams) -> Result<(), DriverError> {
        self.is_up()?;
        if self.scan != Scan::Idle {
            return Err(DriverError::Busy);
        }
        let all: Vec<u16>;
        let channels: &[u16] = if p.channels.is_empty() {
            all = self.channels.iter().map(|c| c.freq as u16).collect();
            &all
        } else {
            &p.channels
        };
        let ssids: Vec<&[u8]> = p.ssids.iter().map(Vec::as_slice).collect();
        const WMI_SCAN_FLAG_PASSIVE: u32 = 0x1;
        const WMI_SCAN_CHAN_STAT_EVENT: u32 = 0x10;
        let mut flags = WMI_SCAN_CHAN_STAT_EVENT;
        if ssids.is_empty() {
            flags |= WMI_SCAN_FLAG_PASSIVE;
        }
        let bssids = [[0xff; 6]];
        let arg = wmi::StartScanArg {
            scan_id: 0, // ATH10K_SCAN_ID
            scan_req_id: 1,
            vdev_id: VDEV_ID,
            scan_priority: 1, // WMI_SCAN_PRIORITY_LOW
            notify_scan_events: wmi::scan_event::STARTED
                | wmi::scan_event::COMPLETED
                | wmi::scan_event::BSS_CHANNEL
                | wmi::scan_event::FOREIGN_CHANNEL
                | wmi::scan_event::FOREIGN_CHANNEL_EXIT
                | wmi::scan_event::DEQUEUED,
            dwell_time_active: 50,
            dwell_time_passive: 150,
            min_rest_time: 50,
            max_rest_time: 500,
            repeat_probe_time: 0,
            probe_spacing_time: 0,
            idle_time: 0,
            max_scan_time: 20000,
            probe_delay: 5,
            scan_ctrl_flags: flags,
            burst_duration_ms: 0,
            ie: &p.ie,
            channels,
            ssids: &ssids,
            bssids: &bssids,
            mac_addr: [0; 6],
            mac_mask: [0; 6],
        };
        let body = wmi::gen_start_scan(&arg).map_err(DriverError::Wmi)?;
        self.queue(cmd::START_SCAN, body, None)?;
        self.scan = Scan::Starting(self.now.saturating_add(timeout::SCAN_START));
        Ok(())
    }

    fn push_stop_scan(&mut self) {
        if let Ok(b) = wmi::gen_stop_scan(1, wmi::StopScan::One { scan_id: 0 }) {
            self.push_q(cmd::STOP_SCAN, b);
        }
    }

    /// `ath10k_scan_abort`. [`Event::ScanDone`] with `aborted` follows. Stopping no scan is a no-op.
    pub fn stop_scan(&mut self) -> Result<(), DriverError> {
        self.is_up()?;
        if let Scan::Running(_) = self.scan {
            self.push_stop_scan();
            self.scan = Scan::Aborting(self.now.saturating_add(timeout::SCAN_STOP));
        }
        Ok(())
    }

    fn queue(&mut self, id: u32, body: Vec<u8>, done: Option<Event>) -> Result<(), DriverError> {
        if self.wmiq.len() >= WMI_QUEUE_MAX {
            return Err(DriverError::Busy);
        }
        self.wmiq.push_back(QueuedCmd {
            frame: wmi::frame(id, &body),
            done,
        });
        Ok(())
    }

    /// `ath10k_vdev_start`, then, on the response, the BSS parameters and the peer as mac80211 orders them.
    /// [`Event::VdevStarted`] or [`Event::JoinFailed`] follows.
    pub fn join(&mut self, p: &JoinParams) -> Result<(), DriverError> {
        self.is_up()?;
        if self.link != Link::Idle {
            return Err(DriverError::Busy);
        }
        let channel = wmi::ChannelArg {
            freq: p.freq,
            band_center_freq1: p.center_freq1,
            mode: p.phymode,
            min_power: 0,
            // TX power and channel power fields unchanged on purpose (owner's rule, audit D6)
            max_power: u32::from(p.max_power_dbm) * 2,
            max_reg_power: u32::from(p.max_power_dbm) * 2,
            allow_ht: false,
            ..wmi::ChannelArg::default()
        };
        let arg = wmi::VdevStartArg {
            vdev_id: VDEV_ID,
            channel,
            // the arvif values: mac80211 starts the channel context before it reports the beacon timing, so a
            // first join carries 0 for both; the AP's values go out as vdev params afterwards
            bcn_intval: self.bcn_intval,
            dtim_period: self.dtim_period,
            ..wmi::VdevStartArg::default()
        };
        let body = wmi::gen_vdev_start(&arg).map_err(DriverError::Wmi)?;
        self.queue(cmd::VDEV_START_REQUEST, body, None)?;
        self.join = *p;
        self.link = Link::WaitVdevStart(self.now.saturating_add(timeout::VDEV));
        Ok(())
    }

    /// The BSS parameters mac80211 reports with the association (`BSS_CHANGED_BEACON_INFO`, `_ERP_SLOT`,
    /// `_ERP_PREAMBLE`, handled before `_ASSOC` in `ath10k_bss_info_changed`), then `ath10k_bss_assoc` up to the point
    /// before `vdev_up`: the peer's rates and capabilities, SMPS, beamforming. One `peer_assoc`: for a station vif
    /// `ath10k_sta_state` does not call `ath10k_station_assoc` (that is for AP, mesh and IBSS vifs), so
    /// `ath10k_bss_assoc` sends the only one. [`Event::PeerAssocDone`] follows. With [`PeerAssoc::bssid`] other than the joined one it is refused.
    pub fn peer_assoc(&mut self, a: &PeerAssoc) -> Result<(), DriverError> {
        self.is_up()?;
        if self.link != Link::Joined {
            return Err(DriverError::WrongState);
        }
        if a.bssid != self.join.bssid {
            return Err(DriverError::Invalid);
        }
        let j = self.join;
        self.dtim_period = j.dtim_period;
        self.push_q(
            cmd::VDEV_SET_PARAM,
            wmi::gen_vdev_set_param(VDEV_ID, wmi::vdev_param::DTIM_PERIOD, j.dtim_period),
        );
        // ieee80211_handle_bss_capability reports slot and preamble only when they differ from the long values
        // ieee80211_reset_erp_info left at interface up or disassociation
        self.erp(j.short_slot, j.short_preamble, false);
        let p = peer::prepare(a, self.caps.num_rf_chains);
        let body = wmi::gen_peer_assoc(&p.arg(a, VDEV_ID)).map_err(DriverError::Wmi)?;
        self.queue(cmd::PEER_ASSOC, body, None)?;
        if let Some(smps) = p.smps {
            self.push_q(
                cmd::PEER_SET_PARAM,
                wmi::gen_peer_set_param(VDEV_ID, &a.bssid, wmi::peer_param::SMPS_STATE, smps),
            );
        }
        self.vht_cap = a.vht.map_or(0, |v| v.cap);
        self.recalc_txbf(self.vht_cap);
        let last = self.wmiq.back_mut().ok_or(DriverError::Busy)?;
        last.done = Some(Event::PeerAssocDone);
        Ok(())
    }

    /// `ath10k_mac_vif_recalc_txbf` (`WMI_TXBF_CONF_AFTER_ASSOC` on TLV firmware): what this end can do towards what
    /// the AP can do. Skipped when the chip has no beamforming capability.
    fn recalc_txbf(&mut self, peer_vht_cap: u32) {
        const SU_BFER: u32 = 1 << 11;
        const SU_BFEE: u32 = 1 << 12;
        const MU_BFER: u32 = 1 << 19;
        const MU_BFEE: u32 = 1 << 20;
        use wmi::vdev_param as vp;
        let ours = self.caps.vht_cap_info;
        if ours & (SU_BFER | SU_BFEE | MU_BFER | MU_BFEE) == 0 {
            return;
        }
        let mut v = 0;
        if ours & (SU_BFEE | MU_BFEE) != 0 {
            if peer_vht_cap & SU_BFER != 0 {
                v |= vp::TXBF_SU_TX_BFEE;
            }
            if peer_vht_cap & MU_BFER != 0 {
                v |= vp::TXBF_MU_TX_BFEE;
            }
        }
        if ours & (SU_BFER | MU_BFER) != 0 {
            if peer_vht_cap & SU_BFEE != 0 {
                v |= vp::TXBF_SU_TX_BFER;
            }
            if peer_vht_cap & MU_BFEE != 0 {
                v |= vp::TXBF_MU_TX_BFER;
            }
        }
        if v & vp::TXBF_MU_TX_BFEE != 0 {
            v |= vp::TXBF_SU_TX_BFEE;
        }
        if v & vp::TXBF_MU_TX_BFER != 0 {
            v |= vp::TXBF_SU_TX_BFER;
        }
        self.push_q(
            cmd::VDEV_SET_PARAM,
            wmi::gen_vdev_set_param(VDEV_ID, vp::TXBF, v),
        );
    }

    /// The rest of `ath10k_bss_assoc`: `peer_stats_info_enable`, `vdev_up` and the power save workaround poke.
    pub fn up(&mut self, aid: u16, bssid: &[u8; 6]) -> Result<(), DriverError> {
        self.is_up()?;
        if self.link != Link::Joined || self.is_up {
            return Err(DriverError::WrongState);
        }
        self.push_q(
            cmd::PDEV_SET_PARAM,
            wmi::gen_pdev_set_param(wmi::pdev_param::PEER_STATS_INFO_ENABLE, 1),
        );
        self.push_q(
            cmd::VDEV_UP,
            wmi::gen_vdev_up(VDEV_ID, u32::from(aid), bssid),
        );
        self.push_q(
            cmd::PEER_SET_PARAM,
            wmi::gen_peer_set_param(VDEV_ID, bssid, wmi::peer_param::DUMMY_VAR, 1),
        );
        self.is_up = true;
        Ok(())
    }

    /// `ath10k_bss_info_changed` for `BSS_CHANGED_ERP_SLOT` and `_ERP_PREAMBLE`, in that order. With `always` both go
    /// out (the reset of `ieee80211_reset_erp_info` flags both as changed); otherwise only a value that differs from
    /// the long default the reset left.
    fn erp(&mut self, short_slot: bool, short_preamble: bool, always: bool) {
        if always || short_slot {
            let slot = if short_slot {
                wmi::VDEV_SLOT_TIME_SHORT
            } else {
                wmi::VDEV_SLOT_TIME_LONG
            };
            self.push_q(
                cmd::VDEV_SET_PARAM,
                wmi::gen_vdev_set_param(VDEV_ID, wmi::vdev_param::SLOT_TIME, slot),
            );
        }
        if always || short_preamble {
            let pre = if short_preamble {
                wmi::VDEV_PREAMBLE_SHORT
            } else {
                wmi::VDEV_PREAMBLE_LONG
            };
            self.push_q(
                cmd::VDEV_SET_PARAM,
                wmi::gen_vdev_set_param(VDEV_ID, wmi::vdev_param::PREAMBLE, pre),
            );
        }
    }

    /// mac80211's four `drv_conf_tx` calls for a new parameter set (`ieee80211_set_wmm_default`,
    /// `ieee80211_mgd_set_link_qos_params`), `acs` in the order BE, BK, VI, VO. mac80211 walks the ACs in its own
    /// order, VO, VI, BE, BK, and each `ath10k_conf_tx` updates its AC in `arvif->wmm_params`, sends the whole set in
    /// one `vdev_wmm_conf` and then the U-APSD calls of `ath10k_conf_tx_uapsd` for that AC. This station asks for no
    /// U-APSD (its WMM element carries QoS info 0), so the U-APSD value stays 0, the rx wake policy WAKE, the tx wake
    /// threshold ALWAYS and the PS-Poll count NO_MAX; with service 27 or 28 the firmware's own trigger is switched off
    /// per AC. During a [`Driver::leave`] the set waits for `vdev_down`, where mac80211's disassociation puts it.
    /// [`DriverError::Busy`] when the queue has no room for all of it; nothing is queued then.
    pub fn set_wmm(&mut self, acs: &[wmi::WmmAc; 4]) -> Result<(), DriverError> {
        self.is_up()?;
        if let Link::WaitPeerGone(_) = self.link {
            self.wmm_after_down = Some(*acs);
            return Ok(());
        }
        if self.wmiq.len() + 4 * 6 > WMI_QUEUE_MAX {
            return Err(DriverError::Busy);
        }
        self.conf_tx_all(acs);
        Ok(())
    }

    fn conf_tx_all(&mut self, acs: &[wmi::WmmAc; 4]) {
        let auto_trig = self.has_svc(wmi::svc::STA_UAPSD_BASIC_AUTO_TRIG)
            || self.has_svc(wmi::svc::STA_UAPSD_VAR_AUTO_TRIG);
        // (index in BE, BK, VI, VO; wmm_ac; user priority) in mac80211's VO, VI, BE, BK order, the pairs as
        // ath10k_conf_tx_uapsd's switch gives them
        for (i, wmm_ac, prio) in [(3usize, 3u32, 7u32), (2, 2, 5), (0, 1, 2), (1, 0, 0)] {
            self.wmm_params[i] = acs[i];
            let params = self.wmm_params;
            self.push_q(
                cmd::VDEV_SET_WMM_PARAMS,
                wmi::gen_vdev_wmm_conf(VDEV_ID, &params),
            );
            for (param, value) in [
                (sta_ps::PARAM_UAPSD, 0),
                (sta_ps::PARAM_RX_WAKE_POLICY, sta_ps::RX_WAKE_POLICY_WAKE),
                (
                    sta_ps::PARAM_TX_WAKE_THRESHOLD,
                    sta_ps::TX_WAKE_THRESHOLD_ALWAYS,
                ),
                (sta_ps::PARAM_PSPOLL_COUNT, sta_ps::PSPOLL_COUNT_NO_MAX),
            ] {
                self.push_q(
                    cmd::STA_POWERSAVE_PARAM,
                    wmi::gen_set_sta_ps(VDEV_ID, param, value),
                );
            }
            if auto_trig {
                let bssid = self.join.bssid;
                self.push_q(
                    cmd::STA_UAPSD_AUTO_TRIG,
                    wmi::gen_vdev_sta_uapsd(VDEV_ID, &bssid, wmm_ac, prio),
                );
            }
        }
    }

    /// `ath10k_set_key`. [`Event::KeyInstalled`] or [`Event::KeyFailed`] follows; keys go in the order given.
    pub fn install_key(&mut self, k: &KeySpec) -> Result<(), DriverError> {
        self.is_up()?;
        if self.link != Link::Joined {
            return Err(DriverError::WrongState);
        }
        let want = match k.cipher {
            Cipher::Ccmp => 16,
            Cipher::Tkip => 32,
        };
        if k.key.len() != want || u32::from(k.key_idx) > wmi::MAX_KEY_INDEX {
            return Err(DriverError::Invalid);
        }
        if self.keys.len() >= 4 {
            return Err(DriverError::Busy);
        }
        self.keys.push_back(k.clone());
        Ok(())
    }

    /// Mac80211's teardown order: `ath10k_peer_delete`, then `ath10k_bss_disassoc`'s `vdev_down`, then `ath10k_vdev_stop`.
    /// [`Event::Disconnected`] with [`Disconnect::Left`] follows.
    pub fn leave(&mut self) -> Result<(), DriverError> {
        self.is_up()?;
        match self.link {
            Link::Joined => {
                let b = self.join.bssid;
                self.push_q(cmd::PEER_DELETE, wmi::gen_peer_delete(VDEV_ID, &b));
                self.link = Link::WaitPeerGone(self.now.saturating_add(timeout::CMD));
                self.keys.clear();
                self.key_wait = None;
                Ok(())
            }
            Link::Idle => {
                self.events.push(Event::Disconnected(Disconnect::Left));
                Ok(())
            }
            _ => Err(DriverError::Busy),
        }
    }

    /// `ath10k_mac_tx` for a management frame. By reference over WMI when the firmware has `WMI_SERVICE_MGMT_TX_WMI`
    /// (`ATH10K_FW_FEATURE_MGMT_TX_BY_REF` is set in this firmware's `firmware-5.bin`), through the HTT tx path on HTT 3
    /// firmware otherwise (`ath10k_mac_tx_h_get_txpath`). A protected action, deauthentication or disassociation frame
    /// gets its 8 bytes of MIC room here. [`Event::MgmtTxDone`] follows.
    pub fn tx_mgmt(&mut self, frame: &[u8]) -> Result<(), DriverError> {
        self.is_up()?;
        if frame.len() < 24 || frame.len() + CCMP_MIC_LEN > MGMT_FRAME_MAX {
            return Err(DriverError::Invalid);
        }
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        let stype = (fc >> 4) & 0xf;
        let room = if fc & 0x4000 != 0 && matches!(stype, 0xd | 0xc | 0xa) {
            CCMP_MIC_LEN
        } else {
            0
        };
        if !self.has_svc(wmi::svc::MGMT_TX_WMI) {
            if self.htt_major < 3 {
                return Err(DriverError::Unsupported);
            }
            let mut f = Vec::with_capacity(frame.len() + room);
            f.extend_from_slice(frame);
            f.resize(frame.len() + room, 0);
            let flags0 = htt::tx_flags0_for(htt::txrx_mode::MGMT, false);
            let flags1 = htt::tx_flags1_for(VDEV_ID as u8, htt::ext_tid::MGMT, false);
            return self.htt_send(TxKind::Mgmt, flags0, flags1, &f).map(|_| ());
        }
        if self.mgmt.len() >= MGMT_PENDING_MAX || self.wmiq.len() >= WMI_QUEUE_MAX {
            return Err(DriverError::Busy);
        }
        let total = frame.len() + room;
        let (_, _, dma) = self.plat.parts();
        let Some(buf) = dma.alloc(total) else {
            return Err(DriverError::NoMem);
        };
        dma.write(&buf, 0, frame);
        if room != 0 {
            dma.write(&buf, frame.len(), &[0; CCMP_MIC_LEN]);
        }
        let desc_id = self.next_desc_id;
        self.next_desc_id = (desc_id + 1) % 0x1_0000;
        let mut f = Vec::with_capacity(total);
        f.extend_from_slice(frame);
        f.resize(total, 0);
        let body = wmi::gen_mgmt_tx_send(VDEV_ID, desc_id, buf.paddr, &f);
        self.mgmt.push(MgmtSlot { desc_id, buf });
        self.push_q(cmd::MGMT_TX_SEND, body);
        Ok(())
    }

    fn htt_send(
        &mut self,
        kind: TxKind,
        flags0: u8,
        flags1: u16,
        frame: &[u8],
    ) -> Result<u16, DriverError> {
        let eid = self.htt_eid;
        let tx = self.tx.as_mut().ok_or(DriverError::NotReady)?;
        tx.send(
            &mut self.plat,
            &mut self.hif,
            eid,
            kind,
            flags0,
            flags1,
            frame,
        )
        .map_err(|e| match e {
            TxError::Busy => DriverError::Busy,
            TxError::TooLong => DriverError::Invalid,
            TxError::Ce(c) => DriverError::Ce(c),
        })
    }

    /// `ath10k_htt_tx_64` for a Native-WiFi frame: `tid` as `ath10k_htt_tx_get_tid` gives it (0-7 for QoS,
    /// [`htt::ext_tid::NON_QOS_MCAST_BCAST`] otherwise); `no_encrypt` is `ATH10K_SKB_F_NO_HWCRYPT` (EAPOL before the
    /// keys). Returns the id of the later [`Event::TxFree`].
    pub fn tx_data(&mut self, frame: &[u8], tid: u8, no_encrypt: bool) -> Result<u16, DriverError> {
        self.is_up()?;
        if self.link != Link::Joined {
            return Err(DriverError::WrongState);
        }
        let flags0 = htt::tx_flags0_for(htt::txrx_mode::NATIVE_WIFI, no_encrypt);
        let flags1 = htt::tx_flags1_for(VDEV_ID as u8, tid, false);
        self.htt_send(TxKind::Data, flags0, flags1, frame)
    }

    /// Whether a frame would be accepted now.
    pub fn can_tx(&self) -> bool {
        self.boot == Boot::Up
            && self.tx.as_ref().is_some_and(HttTx::has_room)
            && self
                .hif
                .ce
                .num_free_src_entries(crate::snoc_config::CE_POLL_PIPE)
                .unwrap_or(0)
                >= 2
    }

    /// MSDUs in flight.
    pub fn tx_pending(&self) -> usize {
        self.tx.as_ref().map_or(0, HttTx::pending)
    }

    /// Buffers waiting in the HTT rx ring (`rx_ring.fill_cnt`).
    pub fn rx_fill(&self) -> usize {
        self.rx.as_ref().map_or(0, HttRx::fill_cnt)
    }
}

const _: () = assert!(CE_COUNT == 12);
