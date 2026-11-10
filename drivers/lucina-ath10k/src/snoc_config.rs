//! The WCN3990 SNOC bus tables: which copy engine carries what, and where the interrupts and shadow registers are.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/snoc.c` (SPDX ISC; Copyright (c) 2018 The Linux Foundation,
//! Qualcomm Technologies, Inc. and/or its subsidiaries), `ce.h` (ISC; Atheros, Qualcomm Atheros, The Linux Foundation),
//! `qmi.h` (`struct ath10k_shadow_reg_cfg`) and `arch/arm64/boot/dts/qcom/sc7180.dtsi` (the `wifi@18800000` node).
//! Every number below is checked against the C compiler's view of those files by `tests/fixtures.rs`, which compares
//! against `tests/fixtures/snoc_oracle.txt` and `dt_snoc.txt` (regenerate with `oracle/gen.sh`).
//!
//! * [`HOST_CE_CONFIG`] is `host_ce_config_wlan[]`: what the host allocates (ring sizes) and which callback each
//!   engine runs. Callbacks are named by [`CeCallback`]; the kernel integration decides what they do.
//! * [`TARGET_CE_CONFIG`] is `target_ce_config_wlan[]` and [`TARGET_SERVICE_TO_CE_MAP`] is
//!   `target_service_to_ce_map_wlan[]`. They go to the firmware over QMI (`ath10k_snoc_wlan_enable`), so their
//!   wire encodings are [`TargetPipeConfig::to_le_bytes`] and [`ServiceToPipe::to_le_bytes`]. The service map
//!   keeps its all-zero terminator entry because `ath10k_snoc_wlan_enable` counts it too.
//! * [`TARGET_SHADOW_REG_CFG`] is `target_shadow_reg_cfg_map[]`, also sent over QMI.
//! * [`map_service_to_pipe`] follows `ath10k_snoc_hif_map_service_to_pipe`.
//! * [`CE_IRQ_SPI`] is the SPI number of CE0..CE11 from the device tree; [`MEMBASE_PHYS`]/[`MEMBASE_SIZE`] is the
//!   `membase` register window.

use crate::htc::svc;

/// Number of copy engines (`CE_COUNT_MAX`, and `wcn3990_values.ce_count`).
pub const CE_COUNT: usize = 12;
/// `reg-names = "membase"` of the `wifi@18800000` node.
pub const MEMBASE_PHYS: u64 = 0x1880_0000;
/// Size of the `membase` window; every register offset in this crate is below it.
pub const MEMBASE_SIZE: usize = 0x80_0000;
/// GIC SPI numbers of the twelve CE interrupts, CE0 first. `platform_get_irq(dev, i)` is CE `i`.
pub const CE_IRQ_SPI: [u32; CE_COUNT] =
    [414, 415, 416, 417, 418, 419, 420, 421, 422, 423, 424, 425];
/// `drv_priv.dma_mask`: the device addresses 35 bits (`DMA_BIT_MASK(35)`).
pub const DMA_MASK_BITS: u32 = 35;
/// `drv_priv.msa_size`.
pub const MSA_SIZE: usize = 0x10_0000;
/// `CE_POLL_PIPE`: the engine whose completions are polled from the HTT tx callbacks.
pub const CE_POLL_PIPE: usize = 4;
/// `ATH10K_SNOC_WAKE_IRQ`: the CE interrupt that is armed as a wake source.
pub const WAKE_IRQ: usize = 2;
/// `ATH10K_SNOC_RX_POST_RETRY_MS`.
pub const RX_POST_RETRY_MS: u32 = 50;

/// `CE_ATTR_*` flags of `struct ce_attr`.
pub mod attr {
    pub const NO_SNOOP: u32 = 1 << 0;
    pub const BYTE_SWAP_DATA: u32 = 1 << 1;
    pub const SWIZZLE_DESCRIPTORS: u32 = 1 << 2;
    /// No interrupt on copy completion (the host polls; also switches the read-index source to RRI memory).
    pub const DIS_INTR: u32 = 1 << 3;
    pub const POLL: u32 = 1 << 4;
    /// `CE_ATTR_FLAGS`: what every engine but CE4 uses.
    pub const DEFAULT: u32 = 0;
}

/// `PIPEDIR_*`, relative to the host.
pub mod pipedir {
    pub const NONE: u32 = 0;
    /// Target to host (the WiFi receive direction).
    pub const IN: u32 = 1;
    /// Host to target.
    pub const OUT: u32 = 2;
    pub const INOUT: u32 = 3;
}

/// The `ath10k_snoc_*_cb` functions `host_ce_config_wlan[]` points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeCallback {
    /// `ath10k_snoc_htc_tx_cb`
    HtcTx,
    /// `ath10k_snoc_htt_tx_cb`
    HttTx,
    /// `ath10k_snoc_htc_rx_cb`
    HtcRx,
    /// `ath10k_snoc_htt_rx_cb`
    HttRx,
    /// `ath10k_snoc_htt_htc_rx_cb`
    HttHtcRx,
    /// `ath10k_snoc_pktlog_rx_cb`
    PktlogRx,
}

impl CeCallback {
    /// The name the C oracle prints.
    pub const fn name(self) -> &'static str {
        match self {
            CeCallback::HtcTx => "htc_tx",
            CeCallback::HttTx => "htt_tx",
            CeCallback::HtcRx => "htc_rx",
            CeCallback::HttRx => "htt_rx",
            CeCallback::HttHtcRx => "htt_htc_rx",
            CeCallback::PktlogRx => "pktlog_rx",
        }
    }
}

/// `struct ce_attr`: the host's view of one copy engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CeAttr {
    pub flags: u32,
    pub src_nentries: u32,
    pub src_sz_max: u32,
    pub dest_nentries: u32,
    pub send_cb: Option<CeCallback>,
    pub recv_cb: Option<CeCallback>,
}

const fn host(
    flags: u32,
    src_nentries: u32,
    src_sz_max: u32,
    dest_nentries: u32,
    send_cb: Option<CeCallback>,
    recv_cb: Option<CeCallback>,
) -> CeAttr {
    CeAttr {
        flags,
        src_nentries,
        src_sz_max,
        dest_nentries,
        send_cb,
        recv_cb,
    }
}

/// `host_ce_config_wlan[]`.
pub static HOST_CE_CONFIG: [CeAttr; CE_COUNT] = [
    // CE0: host->target HTC control streams
    host(attr::DEFAULT, 16, 2048, 0, Some(CeCallback::HtcTx), None),
    // CE1: target->host HTT + HTC control
    host(
        attr::DEFAULT,
        0,
        2048,
        512,
        None,
        Some(CeCallback::HttHtcRx),
    ),
    // CE2: target->host WMI
    host(attr::DEFAULT, 0, 2048, 64, None, Some(CeCallback::HtcRx)),
    // CE3: host->target WMI
    host(attr::DEFAULT, 32, 2048, 0, Some(CeCallback::HtcTx), None),
    // CE4: host->target HTT
    host(
        attr::DEFAULT | attr::DIS_INTR,
        2048,
        256,
        0,
        Some(CeCallback::HttTx),
        None,
    ),
    // CE5: target->host HTT (ipa_uc->target)
    host(attr::DEFAULT, 0, 512, 512, None, Some(CeCallback::HttRx)),
    // CE6: target autonomous hif_memcpy
    host(attr::DEFAULT, 0, 0, 0, None, None),
    // CE7: ce_diag, the Diagnostic Window
    host(attr::DEFAULT, 2, 2048, 2, None, None),
    // CE8: target to uMC
    host(attr::DEFAULT, 0, 2048, 128, None, None),
    // CE9: target->host HTT
    host(
        attr::DEFAULT,
        0,
        2048,
        512,
        None,
        Some(CeCallback::HttHtcRx),
    ),
    // CE10: target->host HTT
    host(
        attr::DEFAULT,
        0,
        2048,
        512,
        None,
        Some(CeCallback::HttHtcRx),
    ),
    // CE11: target->host PKTLOG
    host(
        attr::DEFAULT,
        0,
        2048,
        512,
        None,
        Some(CeCallback::PktlogRx),
    ),
];

/// `struct ce_pipe_config` (`ath10k_tgt_pipe_cfg` over QMI): what the firmware is told about each engine.
/// Note that these ring sizes differ from the host's; the two sides size their own rings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetPipeConfig {
    pub pipenum: u32,
    pub pipedir: u32,
    pub nentries: u32,
    pub nbytes_max: u32,
    pub flags: u32,
    pub reserved: u32,
}

impl TargetPipeConfig {
    /// Six little-endian `u32`, as `struct ce_pipe_config` / `struct ath10k_tgt_pipe_cfg` are laid out.
    pub fn to_le_bytes(&self) -> [u8; 24] {
        let mut out = [0u8; 24];
        let words = [
            self.pipenum,
            self.pipedir,
            self.nentries,
            self.nbytes_max,
            self.flags,
            self.reserved,
        ];
        for (i, w) in words.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

const fn tgt(
    pipenum: u32,
    pipedir: u32,
    nentries: u32,
    nbytes_max: u32,
    flags: u32,
) -> TargetPipeConfig {
    TargetPipeConfig {
        pipenum,
        pipedir,
        nentries,
        nbytes_max,
        flags,
        reserved: 0,
    }
}

/// `target_ce_config_wlan[]`. CE7's direction is the literal `4` in the C (not one of `PIPEDIR_*`), kept as is.
pub static TARGET_CE_CONFIG: [TargetPipeConfig; CE_COUNT] = [
    tgt(0, pipedir::OUT, 32, 2048, attr::DEFAULT),
    tgt(1, pipedir::IN, 32, 2048, attr::DEFAULT),
    tgt(2, pipedir::IN, 64, 2048, attr::DEFAULT),
    tgt(3, pipedir::OUT, 32, 2048, attr::DEFAULT),
    tgt(4, pipedir::OUT, 256, 256, attr::DEFAULT | attr::DIS_INTR),
    tgt(5, pipedir::OUT, 1024, 64, attr::DEFAULT | attr::DIS_INTR),
    tgt(6, pipedir::INOUT, 32, 16384, attr::DEFAULT),
    tgt(7, 4, 0, 0, attr::DEFAULT | attr::DIS_INTR),
    tgt(8, pipedir::IN, 32, 2048, 0),
    tgt(9, pipedir::IN, 32, 2048, attr::DEFAULT),
    tgt(10, pipedir::IN, 32, 2048, attr::DEFAULT),
    tgt(11, pipedir::IN, 32, 2048, attr::DEFAULT),
];

/// `struct ce_service_to_pipe`: one direction of one HTC service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceToPipe {
    pub service_id: u32,
    pub pipedir: u32,
    pub pipenum: u32,
}

impl ServiceToPipe {
    /// Three little-endian `u32` (`struct ath10k_svc_pipe_cfg`).
    pub fn to_le_bytes(&self) -> [u8; 12] {
        let mut out = [0u8; 12];
        out[0..4].copy_from_slice(&self.service_id.to_le_bytes());
        out[4..8].copy_from_slice(&self.pipedir.to_le_bytes());
        out[8..12].copy_from_slice(&self.pipenum.to_le_bytes());
        out
    }
}

const fn s2p(service_id: u32, pipedir: u32, pipenum: u32) -> ServiceToPipe {
    ServiceToPipe {
        service_id,
        pipedir,
        pipenum,
    }
}

/// `target_service_to_ce_map_wlan[]`, terminator included.
///
/// Three services (HTT_DATA2, HTT_DATA3, HTT_LOG) only have a target-to-host entry, so
/// [`map_service_to_pipe`] refuses them: the SNOC driver cannot connect them through HTC.
pub static TARGET_SERVICE_TO_CE_MAP: [ServiceToPipe; 21] = [
    s2p(svc::WMI_DATA_VO as u32, pipedir::OUT, 3),
    s2p(svc::WMI_DATA_VO as u32, pipedir::IN, 2),
    s2p(svc::WMI_DATA_BK as u32, pipedir::OUT, 3),
    s2p(svc::WMI_DATA_BK as u32, pipedir::IN, 2),
    s2p(svc::WMI_DATA_BE as u32, pipedir::OUT, 3),
    s2p(svc::WMI_DATA_BE as u32, pipedir::IN, 2),
    s2p(svc::WMI_DATA_VI as u32, pipedir::OUT, 3),
    s2p(svc::WMI_DATA_VI as u32, pipedir::IN, 2),
    s2p(svc::WMI_CONTROL as u32, pipedir::OUT, 3),
    s2p(svc::WMI_CONTROL as u32, pipedir::IN, 2),
    s2p(svc::RSVD_CTRL as u32, pipedir::OUT, 0),
    s2p(svc::RSVD_CTRL as u32, pipedir::IN, 2),
    // "not used"
    s2p(svc::TEST_RAW_STREAMS as u32, pipedir::OUT, 0),
    s2p(svc::TEST_RAW_STREAMS as u32, pipedir::IN, 2),
    s2p(svc::HTT_DATA_MSG as u32, pipedir::OUT, 4),
    s2p(svc::HTT_DATA_MSG as u32, pipedir::IN, 1),
    // "not used"; a second OUT entry for the same service, which wins in the lookup (C has a WARN_ON for it)
    s2p(svc::TEST_RAW_STREAMS as u32, pipedir::OUT, 5),
    s2p(svc::HTT_DATA2_MSG as u32, pipedir::IN, 9),
    s2p(svc::HTT_DATA3_MSG as u32, pipedir::IN, 10),
    s2p(svc::HTT_LOG_MSG as u32, pipedir::IN, 11),
    // must be last
    s2p(0, 0, 0),
];

/// `struct ath10k_shadow_reg_cfg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowRegCfg {
    pub ce_id: u16,
    pub reg_offset: u16,
}

impl ShadowRegCfg {
    pub fn to_le_bytes(&self) -> [u8; 4] {
        let a = self.ce_id.to_le_bytes();
        let b = self.reg_offset.to_le_bytes();
        [a[0], a[1], b[0], b[1]]
    }
}

/// `WCN3990_SRC_WR_IDX_OFFSET`.
pub const WCN3990_SRC_WR_IDX_OFFSET: u16 = 0x3c;
/// `WCN3990_DST_WR_IDX_OFFSET`.
pub const WCN3990_DST_WR_IDX_OFFSET: u16 = 0x40;

const fn sh(ce_id: u16, reg_offset: u16) -> ShadowRegCfg {
    ShadowRegCfg { ce_id, reg_offset }
}

/// `target_shadow_reg_cfg_map[]`.
pub static TARGET_SHADOW_REG_CFG: [ShadowRegCfg; 12] = [
    sh(0, WCN3990_SRC_WR_IDX_OFFSET),
    sh(3, WCN3990_SRC_WR_IDX_OFFSET),
    sh(4, WCN3990_SRC_WR_IDX_OFFSET),
    sh(5, WCN3990_SRC_WR_IDX_OFFSET),
    sh(7, WCN3990_SRC_WR_IDX_OFFSET),
    sh(1, WCN3990_DST_WR_IDX_OFFSET),
    sh(2, WCN3990_DST_WR_IDX_OFFSET),
    sh(7, WCN3990_DST_WR_IDX_OFFSET),
    sh(8, WCN3990_DST_WR_IDX_OFFSET),
    sh(9, WCN3990_DST_WR_IDX_OFFSET),
    sh(10, WCN3990_DST_WR_IDX_OFFSET),
    sh(11, WCN3990_DST_WR_IDX_OFFSET),
];

/// The pair of copy engines a service uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PipePair {
    /// Host to target.
    pub ul_pipe: u8,
    /// Target to host.
    pub dl_pipe: u8,
}

/// The service has no entry in both directions (`-ENOENT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoSuchService;

/// `ath10k_snoc_hif_map_service_to_pipe`: scan the whole table; for every entry of the service, the last one per
/// direction wins; both directions must be present.
pub fn map_service_to_pipe(service_id: u16) -> Result<PipePair, NoSuchService> {
    let mut ul: Option<u8> = None;
    let mut dl: Option<u8> = None;
    for e in TARGET_SERVICE_TO_CE_MAP.iter() {
        if e.service_id != u32::from(service_id) {
            continue;
        }
        match e.pipedir {
            pipedir::IN => dl = Some(e.pipenum as u8),
            pipedir::OUT => ul = Some(e.pipenum as u8),
            pipedir::INOUT => {
                dl = Some(e.pipenum as u8);
                ul = Some(e.pipenum as u8);
            }
            _ => {}
        }
    }
    match (ul, dl) {
        (Some(ul_pipe), Some(dl_pipe)) => Ok(PipePair { ul_pipe, dl_pipe }),
        _ => Err(NoSuchService),
    }
}

/// `ath10k_snoc_hif_get_default_pipe`: the pipes of the HTC pseudo control service.
pub fn default_pipe() -> Result<PipePair, NoSuchService> {
    map_service_to_pipe(svc::RSVD_CTRL)
}
