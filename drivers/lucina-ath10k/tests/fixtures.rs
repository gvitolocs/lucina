//! Checks the tables, register values and wire formats of this crate against numbers that gen.sh extracted from the
//! Linux source with a C compiler (tests/fixtures/*.txt).
//! The expected values derive from Linux ath10k (SPDX ISC), see oracle/README.md.

mod common;

use common::{hex, kv, kvl, num, to_hex};
use lucina_ath10k::ce::{self, hw};
use lucina_ath10k::htc::{self, Event, Hdr, Htc, State, flags, msg, svc};
use lucina_ath10k::snoc_config::{self as snoc, CE_IRQ_SPI, HOST_CE_CONFIG, TARGET_CE_CONFIG};

const SNOC: &str = include_str!("fixtures/snoc_oracle.txt");
const DT: &str = include_str!("fixtures/dt_snoc.txt");
const HW: &str = include_str!("fixtures/hw_oracle.txt");
const HTC: &str = include_str!("fixtures/htc_oracle.txt");

fn lines_with<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.starts_with(prefix)).collect()
}

#[test]
fn snoc_tables_match_snoc_c() {
    let mut want: Vec<String> = Vec::new();
    want.push(format!("host_ce.count {}", HOST_CE_CONFIG.len()));
    for (i, a) in HOST_CE_CONFIG.iter().enumerate() {
        want.push(format!(
            "host_ce.{i} flags={} src_nentries={} src_sz_max={} dest_nentries={} send_cb={} recv_cb={}",
            a.flags,
            a.src_nentries,
            a.src_sz_max,
            a.dest_nentries,
            a.send_cb.map_or("none", |c| c.name()),
            a.recv_cb.map_or("none", |c| c.name())
        ));
    }
    want.push(format!("target_ce.count {}", TARGET_CE_CONFIG.len()));
    for (i, t) in TARGET_CE_CONFIG.iter().enumerate() {
        want.push(format!(
            "target_ce.{i} pipenum={} pipedir={} nentries={} nbytes_max={} flags={} reserved={}",
            t.pipenum, t.pipedir, t.nentries, t.nbytes_max, t.flags, t.reserved
        ));
    }
    want.push(format!(
        "svc_map.count {}",
        snoc::TARGET_SERVICE_TO_CE_MAP.len()
    ));
    for (i, s) in snoc::TARGET_SERVICE_TO_CE_MAP.iter().enumerate() {
        want.push(format!(
            "svc_map.{i} service_id=0x{:x} pipedir={} pipenum={}",
            s.service_id, s.pipedir, s.pipenum
        ));
    }
    want.push(format!(
        "shadow.count {}",
        snoc::TARGET_SHADOW_REG_CFG.len()
    ));
    for (i, s) in snoc::TARGET_SHADOW_REG_CFG.iter().enumerate() {
        want.push(format!(
            "shadow.{i} ce_id={} reg_offset=0x{:x}",
            s.ce_id, s.reg_offset
        ));
    }
    let got: Vec<String> = ["host_ce.", "target_ce.", "svc_map.", "shadow."]
        .iter()
        .flat_map(|p| lines_with(SNOC, p))
        .map(String::from)
        .collect();
    assert_eq!(got, want);

    let k = kv(SNOC);
    assert_eq!(num(&k["sizeof.ce_pipe_config"]), 24);
    assert_eq!(TARGET_CE_CONFIG[0].to_le_bytes().len(), 24);
    assert_eq!(num(&k["sizeof.ce_service_to_pipe"]), 12);
    assert_eq!(num(&k["sizeof.ath10k_shadow_reg_cfg"]), 4);
    assert_eq!(num(&k["const.PIPEDIR_IN"]) as u32, snoc::pipedir::IN);
    assert_eq!(num(&k["const.PIPEDIR_OUT"]) as u32, snoc::pipedir::OUT);
    assert_eq!(num(&k["const.PIPEDIR_INOUT"]) as u32, snoc::pipedir::INOUT);
    assert_eq!(num(&k["const.PIPEDIR_NONE"]) as u32, snoc::pipedir::NONE);
    assert_eq!(
        num(&k["const.CE_ATTR_NO_SNOOP"]) as u32,
        snoc::attr::NO_SNOOP
    );
    assert_eq!(
        num(&k["const.CE_ATTR_BYTE_SWAP_DATA"]) as u32,
        snoc::attr::BYTE_SWAP_DATA
    );
    assert_eq!(
        num(&k["const.CE_ATTR_SWIZZLE_DESCRIPTORS"]) as u32,
        snoc::attr::SWIZZLE_DESCRIPTORS
    );
    assert_eq!(
        num(&k["const.CE_ATTR_DIS_INTR"]) as u32,
        snoc::attr::DIS_INTR
    );
    assert_eq!(num(&k["const.CE_ATTR_POLL"]) as u32, snoc::attr::POLL);
    assert_eq!(num(&k["const.CE_ATTR_FLAGS"]) as u32, snoc::attr::DEFAULT);
    assert_eq!(
        num(&k["const.WCN3990_SRC_WR_IDX_OFFSET"]) as u16,
        snoc::WCN3990_SRC_WR_IDX_OFFSET
    );
    assert_eq!(
        num(&k["const.WCN3990_DST_WR_IDX_OFFSET"]) as u16,
        snoc::WCN3990_DST_WR_IDX_OFFSET
    );

    for (name, v) in [
        ("RSVD_CTRL", svc::RSVD_CTRL),
        ("WMI_CONTROL", svc::WMI_CONTROL),
        ("WMI_DATA_BE", svc::WMI_DATA_BE),
        ("WMI_DATA_BK", svc::WMI_DATA_BK),
        ("WMI_DATA_VI", svc::WMI_DATA_VI),
        ("WMI_DATA_VO", svc::WMI_DATA_VO),
        ("NMI_CONTROL", svc::NMI_CONTROL),
        ("NMI_DATA", svc::NMI_DATA),
        ("HTT_DATA_MSG", svc::HTT_DATA_MSG),
        ("HTT_DATA2_MSG", svc::HTT_DATA2_MSG),
        ("HTT_DATA3_MSG", svc::HTT_DATA3_MSG),
        ("HTT_LOG_MSG", svc::HTT_LOG_MSG),
        ("TEST_RAW_STREAMS", svc::TEST_RAW_STREAMS),
    ] {
        assert_eq!(num(&k[&format!("svc.{name}")]) as u16, v, "{name}");
    }

    // the service lookup, for every id the oracle tried
    for line in lines_with(SNOC, "map.") {
        let (id, rest) = line["map.".len()..].split_once(' ').unwrap();
        let id = num(id) as u16;
        let r = snoc::map_service_to_pipe(id);
        match rest {
            "enoent" => assert_eq!(r, Err(snoc::NoSuchService), "{id:#x}"),
            _ => {
                let (ul, dl) = rest.split_once(' ').unwrap();
                let (ul, dl) = (
                    ul["ul=".len()..].parse::<u8>().unwrap(),
                    dl["dl=".len()..].parse::<u8>().unwrap(),
                );
                assert_eq!(
                    r,
                    Ok(snoc::PipePair {
                        ul_pipe: ul,
                        dl_pipe: dl
                    }),
                    "{id:#x}"
                );
            }
        }
    }
    assert!(!Htc::pktlog_supported());
}

#[test]
fn device_tree_and_snoc_constants() {
    let k = kv(DT);
    assert_eq!(
        num(k["membase"].split(' ').next().unwrap()),
        snoc::MEMBASE_PHYS
    );
    assert_eq!(
        num(k["membase"].split(' ').nth(1).unwrap()) as usize,
        snoc::MEMBASE_SIZE
    );
    for (i, spi) in CE_IRQ_SPI.iter().enumerate() {
        assert_eq!(num(&k[&format!("irq.ce{i}")]) as u32, *spi, "CE{i}");
    }
    assert_eq!(
        k.keys().filter(|x| x.starts_with("irq.ce")).count(),
        snoc::CE_COUNT
    );
    assert_eq!(num(&k["snoc.CE_POLL_PIPE"]) as usize, snoc::CE_POLL_PIPE);
    assert_eq!(
        num(&k["snoc.ATH10K_SNOC_WAKE_IRQ"]) as usize,
        snoc::WAKE_IRQ
    );
    assert_eq!(
        num(&k["snoc.ATH10K_SNOC_RX_POST_RETRY_MS"]) as u32,
        snoc::RX_POST_RETRY_MS
    );
    assert_eq!(num(&k["snoc.dma_mask_bits"]) as u32, snoc::DMA_MASK_BITS);
    assert_eq!(num(&k["snoc.msa_size"]) as usize, snoc::MSA_SIZE);
}

#[test]
fn ce_registers_match_hw_c() {
    let k = kv(HW);
    let g = |n: &str| num(&k[n]);
    let checks: &[(&str, u64)] = &[
        ("c->sr_base_addr_lo", hw::SR_BASE_ADDR_LO as u64),
        ("c->sr_base_addr_hi", hw::SR_BASE_ADDR_HI as u64),
        ("c->sr_size_addr", hw::SR_SIZE as u64),
        ("c->dr_base_addr_lo", hw::DR_BASE_ADDR_LO as u64),
        ("c->dr_base_addr_hi", hw::DR_BASE_ADDR_HI as u64),
        ("c->dr_size_addr", hw::DR_SIZE as u64),
        ("c->misc_ie_addr", hw::MISC_IE as u64),
        ("c->sr_wr_index_addr", hw::SR_WR_INDEX as u64),
        ("c->dst_wr_index_addr", hw::DST_WR_INDEX as u64),
        ("c->current_srri_addr", hw::CURRENT_SRRI as u64),
        ("c->current_drri_addr", hw::CURRENT_DRRI as u64),
        ("c->ce_rri_low", hw::CE_RRI_LOW as u64),
        ("c->ce_rri_high", hw::CE_RRI_HIGH as u64),
        ("c->host_ie_addr", hw::HOST_IE as u64),
        ("c->ctrl1_regs->addr", hw::CTRL1 as u64),
        (
            "c->ctrl1_regs->src_ring->lsb",
            u64::from(hw::CTRL1_SRC_RING_LSB),
        ),
        (
            "c->ctrl1_regs->src_ring->mask",
            u64::from(hw::CTRL1_SRC_RING_MASK),
        ),
        (
            "c->ctrl1_regs->dst_ring->lsb",
            u64::from(hw::CTRL1_DST_RING_LSB),
        ),
        (
            "c->ctrl1_regs->dst_ring->mask",
            u64::from(hw::CTRL1_DST_RING_MASK),
        ),
        ("c->ctrl1_regs->dmax->lsb", u64::from(hw::CTRL1_DMAX_LSB)),
        ("c->ctrl1_regs->dmax->mask", u64::from(hw::CTRL1_DMAX_MASK)),
        (
            "c->host_ie->copy_complete->mask",
            u64::from(hw::HOST_IE_COPY_COMPLETE),
        ),
        ("c->wm_regs->dstr_lmask", u64::from(hw::WM_DSTR_LMASK)),
        ("c->wm_regs->dstr_hmask", u64::from(hw::WM_DSTR_HMASK)),
        ("c->wm_regs->srcr_lmask", u64::from(hw::WM_SRCR_LMASK)),
        ("c->wm_regs->srcr_hmask", u64::from(hw::WM_SRCR_HMASK)),
        ("c->wm_regs->cc_mask", u64::from(hw::WM_CC_MASK)),
        ("c->wm_regs->wm_mask", u64::from(hw::WM_MASK)),
        ("c->wm_regs->addr", hw::HOST_IS as u64),
        ("c->misc_regs->err_mask", u64::from(hw::MISC_ERR_MASK)),
        ("c->wm_srcr->addr", hw::SRC_WM as u64),
        ("c->wm_srcr->wm_low->lsb", u64::from(hw::WM_LOW_LSB)),
        ("c->wm_srcr->wm_low->mask", u64::from(hw::WM_LOW_MASK)),
        ("c->wm_srcr->wm_high->lsb", u64::from(hw::WM_HIGH_LSB)),
        ("c->wm_srcr->wm_high->mask", u64::from(hw::WM_HIGH_MASK)),
        ("c->wm_dstr->addr", hw::DST_WM as u64),
        ("c->wm_dstr->wm_low->lsb", u64::from(hw::WM_LOW_LSB)),
        ("c->wm_dstr->wm_low->mask", u64::from(hw::WM_LOW_MASK)),
        ("c->wm_dstr->wm_high->lsb", u64::from(hw::WM_HIGH_LSB)),
        ("c->wm_dstr->wm_high->mask", u64::from(hw::WM_HIGH_MASK)),
        ("c->upd->mask", u64::from(hw::CTRL1_UPD_MASK)),
        ("r->ce_wrapper_base_address", hw::CE_WRAPPER_BASE as u64),
        ("r->ce0_base_address", hw::ce_base(0) as u64),
        ("r->ce1_base_address", hw::ce_base(1) as u64),
        ("r->ce2_base_address", hw::ce_base(2) as u64),
        ("r->ce3_base_address", hw::ce_base(3) as u64),
        ("r->ce4_base_address", hw::ce_base(4) as u64),
        ("r->ce5_base_address", hw::ce_base(5) as u64),
        ("r->ce6_base_address", hw::ce_base(6) as u64),
        ("r->ce7_base_address", hw::ce_base(7) as u64),
        ("r->ce8_base_address", hw::ce_base(8) as u64),
        ("r->ce9_base_address", hw::ce_base(9) as u64),
        ("r->ce10_base_address", hw::ce_base(10) as u64),
        ("r->ce11_base_address", hw::ce_base(11) as u64),
        (
            "r->ce_wrap_intr_sum_host_msi_lsb",
            u64::from(hw::INTR_SUM_HOST_MSI_LSB),
        ),
        (
            "r->ce_wrap_intr_sum_host_msi_mask",
            u64::from(hw::INTR_SUM_HOST_MSI_MASK),
        ),
        ("v->ce_count", snoc::CE_COUNT as u64),
        (
            "v->ce_desc_meta_data_mask",
            u64::from(hw::DESC_META_DATA_MASK),
        ),
        (
            "v->ce_desc_meta_data_lsb",
            u64::from(hw::DESC_META_DATA_LSB),
        ),
    ];
    for (name, want) in checks {
        assert_eq!(g(name), *want, "{name}");
    }
    // facts about hw_params the code relies on
    assert_eq!(g("p.target_64bit"), 1, "64-bit descriptors");
    assert_eq!(g("p.shadow_reg_support"), 1, "shadow write index registers");
    assert_eq!(g("p.rri_on_ddr"), 1, "read indices in host memory");
    assert_eq!(g("p.use_fw_tx_credits"), 0, "htc starts with one credit");
    assert_eq!(g("p.delay_unmap_buffer"), 1);
    assert_eq!(g("p.continuous_frag_desc"), 1);
    assert_eq!(g("p.dev_id"), 0);
    // destination CE5 and the dest-ring check are only specific to 32-bit rings
    assert_eq!(ce::DESC_SIZE, 16);
    assert_eq!(ce::hw::shadow_sr_wr_index(0), Some(0x32000));
}

// ---- HTC -----------------------------------------------------------------------------------------------------

#[test]
fn htc_layout_and_constants_match_htc_h() {
    let k = kvl(HTC);
    let g = |n: &str| num(&k[n]);
    assert_eq!(g("sizeof.struct ath10k_htc_hdr") as usize, htc::HDR_LEN);
    assert_eq!(g("offsetof.struct ath10k_htc_hdr.eid"), 0);
    assert_eq!(g("offsetof.struct ath10k_htc_hdr.flags"), 1);
    assert_eq!(g("offsetof.struct ath10k_htc_hdr.len"), 2);
    assert_eq!(g("offsetof.struct ath10k_htc_hdr.trailer_len"), 4);
    assert_eq!(g("offsetof.struct ath10k_htc_hdr.seq_no"), 5);
    assert_eq!(g("offsetof.struct ath10k_htc_hdr.pad_len"), 6);
    assert_eq!(
        g("sizeof.struct ath10k_ath10k_htc_msg_hdr") as usize,
        msg::HDR_LEN
    );
    assert_eq!(g("sizeof.struct ath10k_htc_ready") as usize, msg::READY_LEN);
    assert_eq!(
        g("sizeof.struct ath10k_htc_ready_extended") as usize,
        msg::READY_EXT_LEN
    );
    assert_eq!(
        g("sizeof.struct ath10k_htc_conn_svc") as usize,
        msg::CONNECT_SERVICE_LEN
    );
    assert_eq!(
        g("sizeof.struct ath10k_htc_conn_svc_response") as usize,
        msg::CONNECT_SERVICE_RESP_LEN
    );
    assert_eq!(
        g("sizeof.struct ath10k_htc_setup_complete_extended") as usize,
        msg::SETUP_COMPLETE_EXT_LEN
    );
    assert_eq!(g("sizeof.struct ath10k_htc_msg"), 12);
    assert_eq!(
        g("sizeof.struct ath10k_ath10k_htc_record_hdr") as usize,
        htc::RECORD_HDR_LEN
    );
    assert_eq!(g("sizeof.struct ath10k_htc_credit_report"), 4);
    assert_eq!(g("sizeof.struct ath10k_htc_lookahead_report"), 12);
    assert_eq!(
        g("offsetof.struct ath10k_htc_lookahead_report.lookahead"),
        4
    );
    assert_eq!(
        g("offsetof.struct ath10k_htc_lookahead_report.post_valid"),
        8
    );
    assert_eq!(g("sizeof.struct ath10k_htc_lookahead_bundle"), 4);
    assert_eq!(g("offsetof.struct ath10k_htc_ready.credit_count"), 0);
    assert_eq!(g("offsetof.struct ath10k_htc_ready.credit_size"), 2);
    assert_eq!(g("offsetof.struct ath10k_htc_ready.max_endpoints"), 4);
    assert_eq!(
        g("offsetof.struct ath10k_htc_ready_extended.htc_version"),
        6
    );
    assert_eq!(
        g("offsetof.struct ath10k_htc_ready_extended.max_msgs_per_htc_bundle"),
        7
    );
    assert_eq!(g("offsetof.struct ath10k_htc_ready_extended.reserved"), 8);
    assert_eq!(g("offsetof.struct ath10k_htc_conn_svc.service_id"), 0);
    assert_eq!(g("offsetof.struct ath10k_htc_conn_svc.flags"), 2);
    assert_eq!(
        g("offsetof.struct ath10k_htc_conn_svc_response.service_id"),
        0
    );
    assert_eq!(g("offsetof.struct ath10k_htc_conn_svc_response.status"), 2);
    assert_eq!(g("offsetof.struct ath10k_htc_conn_svc_response.eid"), 3);
    assert_eq!(
        g("offsetof.struct ath10k_htc_conn_svc_response.max_msg_size"),
        4
    );
    assert_eq!(g("offsetof.struct ath10k_htc_msg.ready"), 2);

    let consts: &[(&str, u64)] = &[
        (
            "ATH10K_HTC_FLAG_NEED_CREDIT_UPDATE",
            u64::from(flags::NEED_CREDIT_UPDATE),
        ),
        ("ATH10K_HTC_FLAG_SEND_BUNDLE", u64::from(flags::SEND_BUNDLE)),
        (
            "ATH10K_HTC_FLAGS_RECV_1MORE_BLOCK",
            u64::from(flags::RECV_1MORE_BLOCK),
        ),
        (
            "ATH10K_HTC_FLAG_TRAILER_PRESENT",
            u64::from(flags::TRAILER_PRESENT),
        ),
        ("ATH10K_HTC_FLAG_BUNDLE_MASK", u64::from(flags::BUNDLE_MASK)),
        (
            "ATH10K_HTC_BUNDLE_EXTRA_MASK",
            u64::from(flags::BUNDLE_EXTRA_MASK),
        ),
        (
            "HTC_HOST_MAX_MSG_PER_RX_BUNDLE",
            u64::from(htc::HOST_MAX_MSG_PER_RX_BUNDLE),
        ),
        ("ATH10K_HTC_MSG_READY_ID", u64::from(msg::READY_ID)),
        (
            "ATH10K_HTC_MSG_CONNECT_SERVICE_ID",
            u64::from(msg::CONNECT_SERVICE_ID),
        ),
        (
            "ATH10K_HTC_MSG_CONNECT_SERVICE_RESP_ID",
            u64::from(msg::CONNECT_SERVICE_RESP_ID),
        ),
        (
            "ATH10K_HTC_MSG_SETUP_COMPLETE_ID",
            u64::from(msg::SETUP_COMPLETE_ID),
        ),
        (
            "ATH10K_HTC_MSG_SETUP_COMPLETE_EX_ID",
            u64::from(msg::SETUP_COMPLETE_EX_ID),
        ),
        (
            "ATH10K_HTC_MSG_SEND_SUSPEND_COMPLETE",
            u64::from(msg::SEND_SUSPEND_COMPLETE),
        ),
        ("ATH10K_HTC_VERSION_2P0", u64::from(msg::VERSION_2P0)),
        ("ATH10K_HTC_VERSION_2P1", u64::from(msg::VERSION_2P1)),
        (
            "ATH10K_HTC_CONN_FLAGS_THRESHOLD_LEVEL_MASK",
            u64::from(msg::CONN_FLAGS_THRESHOLD_LEVEL_MASK),
        ),
        (
            "ATH10K_HTC_CONN_FLAGS_REDUCE_CREDIT_DRIBBLE",
            u64::from(msg::CONN_FLAGS_REDUCE_CREDIT_DRIBBLE),
        ),
        (
            "ATH10K_HTC_CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL",
            u64::from(msg::CONN_FLAGS_DISABLE_CREDIT_FLOW_CTRL),
        ),
        (
            "ATH10K_HTC_CONN_FLAGS_RECV_ALLOC_MASK",
            u64::from(msg::CONN_FLAGS_RECV_ALLOC_MASK),
        ),
        (
            "ATH10K_HTC_CONN_FLAGS_RECV_ALLOC_LSB",
            u64::from(msg::CONN_FLAGS_RECV_ALLOC_LSB),
        ),
        (
            "ATH10K_HTC_MSG_READY_EXT_ALT_DATA_MASK",
            u64::from(htc::READY_EXT_ALT_DATA_MASK),
        ),
        (
            "ATH10K_HTC_CONN_SVC_STATUS_SUCCESS",
            u64::from(msg::CONN_SVC_STATUS_SUCCESS),
        ),
        (
            "ATH10K_HTC_CONN_SVC_STATUS_NOT_FOUND",
            u64::from(msg::CONN_SVC_STATUS_NOT_FOUND),
        ),
        (
            "ATH10K_HTC_CONN_SVC_STATUS_FAILED",
            u64::from(msg::CONN_SVC_STATUS_FAILED),
        ),
        (
            "ATH10K_HTC_CONN_SVC_STATUS_NO_RESOURCES",
            u64::from(msg::CONN_SVC_STATUS_NO_RESOURCES),
        ),
        (
            "ATH10K_HTC_CONN_SVC_STATUS_NO_MORE_EP",
            u64::from(msg::CONN_SVC_STATUS_NO_MORE_EP),
        ),
        (
            "ATH10K_HTC_SETUP_COMPLETE_FLAGS_RX_BNDL_EN",
            u64::from(msg::SETUP_COMPLETE_FLAGS_RX_BNDL_EN),
        ),
        ("ATH10K_HTC_RECORD_NULL", u64::from(htc::record::NULL)),
        ("ATH10K_HTC_RECORD_CREDITS", u64::from(htc::record::CREDITS)),
        (
            "ATH10K_HTC_RECORD_LOOKAHEAD",
            u64::from(htc::record::LOOKAHEAD),
        ),
        (
            "ATH10K_HTC_RECORD_LOOKAHEAD_BUNDLE",
            u64::from(htc::record::LOOKAHEAD_BUNDLE),
        ),
        ("ATH10K_HTC_EP_COUNT", htc::EP_COUNT as u64),
        ("ATH10K_HTC_MAX_LEN", htc::MAX_LEN as u64),
        ("ATH10K_HTC_MAX_CTRL_MSG_LEN", htc::MAX_CTRL_MSG_LEN as u64),
        (
            "ATH10K_NUM_CONTROL_TX_BUFFERS",
            htc::NUM_CONTROL_TX_BUFFERS as u64,
        ),
        ("ATH10K_HTC_SVC_ID_RSVD_CTRL", u64::from(svc::RSVD_CTRL)),
        ("ATH10K_HTC_SVC_ID_WMI_CONTROL", u64::from(svc::WMI_CONTROL)),
        (
            "ATH10K_HTC_SVC_ID_HTT_DATA_MSG",
            u64::from(svc::HTT_DATA_MSG),
        ),
        ("ATH10K_HTC_SVC_ID_HTT_LOG_MSG", u64::from(svc::HTT_LOG_MSG)),
    ];
    for (name, want) in consts {
        assert_eq!(g(&format!("const.{name}")), *want, "{name}");
    }
    for line in lines_with(HTC, "bundle_count.") {
        let (key, v) = line.split_once(' ').unwrap();
        let mut it = key["bundle_count.".len()..].split('.');
        let max: u8 = it.next().unwrap().parse().unwrap();
        let fl: u8 = it.next().unwrap().parse::<u32>().unwrap() as u8;
        assert_eq!(
            htc::bundle_count(max, fl),
            v.parse::<u32>().unwrap(),
            "{key}"
        );
    }
}

fn with_hdr(eid: u8, seq: u8, payload: &[u8], flags: u8) -> Vec<u8> {
    let h = Hdr {
        eid,
        flags,
        len: payload.len() as u16,
        control_byte1: seq,
        ..Hdr::default()
    };
    let mut v = h.to_le_bytes().to_vec();
    v.extend_from_slice(payload);
    v
}

#[test]
fn htc_handshake_produces_the_bytes_c_produces() {
    let k = kv(HTC);
    let b = |n: &str| hex(&k[&format!("bytes.{n}")]);

    // headers
    let h = Hdr {
        eid: 1,
        flags: flags::NEED_CREDIT_UPDATE,
        len: 0x123,
        control_byte1: 7,
        ..Hdr::default()
    };
    assert_eq!(h.to_le_bytes().to_vec(), b("hdr_tx"));
    let h = Hdr {
        eid: 2,
        flags: flags::TRAILER_PRESENT,
        len: 0x40,
        control_byte0: 8,
        pad_len: 0x0102,
        ..Hdr::default()
    };
    assert_eq!(h.to_le_bytes().to_vec(), b("hdr_rx"));
    assert_eq!(Hdr::parse(&b("hdr_rx")), Some(h));
    assert_eq!(h.trailer_len(), 8);

    let mut htc = Htc::new().unwrap();
    assert_eq!(htc.state(), State::WaitReady);
    let ep0 = *htc.endpoint(0).unwrap();
    assert_eq!(
        (
            ep0.service_id,
            ep0.ul_pipe_id,
            ep0.dl_pipe_id,
            ep0.tx_credit_flow_enabled
        ),
        (svc::RSVD_CTRL, 0, 2, false)
    );

    // nothing but READY is accepted first; a short one is refused
    assert_eq!(
        htc.connect_service(svc::WMI_CONTROL),
        Err(htc::HtcError::WrongState)
    );
    assert_eq!(
        htc.on_control_message(&b("ready_ext")[..7]),
        Err(htc::HtcError::Protocol)
    );
    let ev = htc.on_control_message(&b("ready_ext")).unwrap();
    let Event::Ready(r) = ev else {
        panic!("{ev:?}")
    };
    assert_eq!(
        u64::from(r.total_transmit_credits),
        num(&k["wait_target.total_transmit_credits"])
    );
    assert_eq!(
        u64::from(r.target_credit_size),
        num(&k["wait_target.target_credit_size"])
    );
    assert_eq!(
        u64::from(r.alt_data_credit_size),
        num(&k["wait_target.alt_data_credit_size"])
    );
    assert_eq!(
        u64::from(r.max_msgs_per_htc_bundle),
        num(&k["wait_target.max_msgs_per_htc_bundle"])
    );
    assert_eq!(
        (
            r.extended,
            r.htc_version,
            r.max_endpoints,
            r.target_credit_count
        ),
        (true, msg::VERSION_2P1, 8, 0x20)
    );

    // WMI control: one credit, flow control on
    let p = htc.connect_service(svc::WMI_CONTROL).unwrap();
    assert_eq!(p.ul_pipe, 0);
    assert_eq!(
        to_hex(&p.bytes),
        to_hex(&with_hdr(0, 0, &b("connect_wmi_control"), 0))
    );
    let Event::Connected(c) = htc.on_control_message(&b("connect_resp")).unwrap() else {
        panic!()
    };
    assert_eq!(
        (c.eid, c.service_id, c.max_msg_len, c.ul_pipe, c.dl_pipe),
        (2, svc::WMI_CONTROL, 0x800, 3, 2)
    );
    let e = *htc.endpoint(2).unwrap();
    assert_eq!(
        (e.tx_credits, e.tx_credit_size, e.tx_credit_flow_enabled),
        (1, 0x800, true)
    );

    // HTT data: no credit flow, alternate credit size
    let p = htc.connect_service(svc::HTT_DATA_MSG).unwrap();
    assert_eq!(
        to_hex(&p.bytes),
        to_hex(&with_hdr(0, 1, &b("connect_htt_data"), 0))
    );
    let mut resp = Vec::new();
    resp.extend_from_slice(&msg::CONNECT_SERVICE_RESP_ID.to_le_bytes());
    resp.extend_from_slice(&svc::HTT_DATA_MSG.to_le_bytes());
    resp.extend_from_slice(&[0, 1]);
    resp.extend_from_slice(&0x800u16.to_le_bytes());
    let Event::Connected(c) = htc.on_control_message(&resp).unwrap() else {
        panic!()
    };
    assert_eq!((c.eid, c.ul_pipe, c.dl_pipe), (1, 4, 1));
    let e = *htc.endpoint(1).unwrap();
    assert_eq!(
        (e.tx_credits, e.tx_credit_size, e.tx_credit_flow_enabled),
        (0, 291, false)
    );
    // endpoint 1 is taken
    assert!(htc.connect_service(svc::WMI_DATA_BE).is_ok());
    resp[4..6].copy_from_slice(&[0, 1]);
    assert_eq!(htc.on_control_message(&resp), Err(htc::HtcError::Protocol));
    assert_eq!(htc.state(), State::Ready);

    // services that are only served target-to-host cannot be connected
    assert!(htc.connect_service(svc::HTT_DATA2_MSG).is_ok());
    resp[2..4].copy_from_slice(&svc::HTT_DATA2_MSG.to_le_bytes());
    resp[4..6].copy_from_slice(&[0, 3]);
    assert_eq!(htc.on_control_message(&resp), Err(htc::HtcError::NoPipe));
    assert_eq!(htc.state(), State::Ready);
    assert_eq!(htc.endpoint(3).unwrap().service_id, svc::RESERVED);

    // refused
    assert!(htc.connect_service(svc::WMI_DATA_BK).is_ok());
    resp[4..6].copy_from_slice(&[msg::CONN_SVC_STATUS_NOT_FOUND, 3]);
    assert_eq!(
        htc.on_control_message(&resp),
        Err(htc::HtcError::ConnectRefused(1))
    );

    // start: SETUP_COMPLETE_EXT with zero flags, sequence number 4 by now
    let p = htc.start().unwrap();
    assert_eq!(
        p.bytes[..8],
        with_hdr(0, 5, &[], 0)[..8]
            .iter()
            .enumerate()
            .map(|(i, x)| if i == 2 { 12 } else { *x })
            .collect::<Vec<u8>>()[..]
    );
    assert_eq!(&p.bytes[8..], &b("setup_complete_ex")[..]);
    assert_eq!(htc.state(), State::Started);
    assert_eq!(htc.start(), Err(htc::HtcError::WrongState));
}

#[test]
fn htc_credits_and_rx_frames() {
    let k = kv(HTC);
    let mut htc = Htc::new().unwrap();
    htc.on_control_message(&hex(&k["bytes.ready_ext"])).unwrap();
    htc.connect_service(svc::WMI_CONTROL).unwrap();
    htc.on_control_message(&hex(&k["bytes.connect_resp"]))
        .unwrap();

    // one credit of 2048 bytes: a 100 byte command takes it, the next waits
    let h = htc.prepare_tx(2, 100).unwrap();
    assert_eq!(
        (h.eid, h.len, h.flags, h.control_byte1),
        (2, 100, flags::NEED_CREDIT_UPDATE, 0)
    );
    assert_eq!(htc.prepare_tx(2, 10), Err(htc::HtcError::NoCredits));
    assert_eq!(
        htc.endpoint(2).unwrap().seq_no,
        1,
        "a refused send does not use a sequence number"
    );
    htc.release_credit(2, 108);
    assert_eq!(htc.endpoint(2).unwrap().tx_credits, 1);
    // spanning two credits needs two
    assert_eq!(htc.prepare_tx(2, 2048), Err(htc::HtcError::NoCredits));
    assert_eq!(htc.prepare_tx(9, 1), Err(htc::HtcError::BadEndpoint));
    assert_eq!(htc.prepare_tx(5, 1), Err(htc::HtcError::BadEndpoint));

    // a frame for the WMI endpoint with a credit report in its trailer
    let mut payload = b"WMIEVENT".to_vec();
    payload.extend_from_slice(&[htc::record::CREDITS, 4, 0, 0, 2, 3, 0, 0]);
    let hdr = Hdr {
        eid: 2,
        flags: flags::TRAILER_PRESENT,
        len: payload.len() as u16,
        control_byte0: 8,
        ..Hdr::default()
    };
    let mut frame = hdr.to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    let before = htc.endpoint(2).unwrap().tx_credits;
    let f = htc.rx_frame(&frame).unwrap();
    assert_eq!((f.eid, f.payload), (2, &b"WMIEVENT"[..]));
    assert_eq!(htc.endpoint(2).unwrap().tx_credits, before + 3);

    // the drop paths
    assert_eq!(htc.rx_frame(&frame[..5]), Err(htc::RxDrop::Short));
    assert_eq!(
        htc.rx_frame(&frame[..frame.len() - 1]),
        Err(htc::RxDrop::Truncated)
    );
    let mut bad = frame.clone();
    bad[0] = 9;
    assert_eq!(htc.rx_frame(&bad), Err(htc::RxDrop::BadEid));
    bad[0] = 5;
    assert_eq!(htc.rx_frame(&bad), Err(htc::RxDrop::NotConnected));
    let mut bad = frame.clone();
    bad[2..4].copy_from_slice(&5000u16.to_le_bytes());
    assert_eq!(htc.rx_frame(&bad), Err(htc::RxDrop::TooLong));
    let mut bad = frame.clone();
    bad[4] = 3;
    assert_eq!(htc.rx_frame(&bad), Err(htc::RxDrop::BadTrailerLen));
    bad[4] = 100;
    assert_eq!(htc.rx_frame(&bad), Err(htc::RxDrop::BadTrailerLen));
    let mut bad = frame.clone();
    bad[8 + 8 + 1] = 40; // record length runs past the trailer
    assert_eq!(
        htc.rx_frame(&bad),
        Err(htc::RxDrop::BadTrailer(htc::HtcError::Invalid))
    );
    // trailer only
    let hdr = Hdr {
        eid: 2,
        flags: flags::TRAILER_PRESENT,
        len: 8,
        control_byte0: 8,
        ..Hdr::default()
    };
    let mut only = hdr.to_le_bytes().to_vec();
    only.extend_from_slice(&[htc::record::CREDITS, 4, 0, 0, 2, 0, 0, 0]);
    assert_eq!(htc.rx_frame(&only), Err(htc::RxDrop::Empty));
    // no trailer: the whole payload
    let hdr = Hdr {
        eid: 2,
        len: 3,
        ..Hdr::default()
    };
    let mut plain = hdr.to_le_bytes().to_vec();
    plain.extend_from_slice(b"abc");
    plain.extend_from_slice(b"padding the buffer is longer than the frame");
    assert_eq!(htc.rx_frame(&plain).unwrap().payload, b"abc");
}

#[test]
fn htc_trailers_match_the_c_function() {
    // the same vectors as oracle/htc_oracle.c; the C answer is in the fixture
    let vectors: Vec<Vec<u8>> = vec![
        vec![1, 8, 0, 0, 1, 2, 0, 0, 3, 7, 0, 0],
        vec![1, 8, 0, 0, 9, 5, 0, 0, 3, 7, 0, 0],
        vec![2, 12, 0, 0, 0x55, 0, 0, 0, 1, 2, 3, 4, 0xaa, 0, 0, 0],
        vec![2, 12, 0, 0, 0x55, 0, 0, 0, 1, 2, 3, 4, 0xab, 0, 0, 0],
        vec![3, 12, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        vec![
            3, 20, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        ],
        vec![3, 3, 0, 0, 1, 2, 3],
        vec![1, 40, 0, 0, 1, 2],
        vec![9, 2, 0, 0, 7, 7, 1, 4, 0, 0, 1, 1, 0, 0],
        vec![1, 2, 0, 0, 1, 2],
        vec![1, 4, 0, 0],
        vec![1, 6, 0, 0, 1, 2, 0, 0, 5, 5],
        vec![1, 0, 0],
    ];
    let mut ready = hex(&kv(HTC)["bytes.ready_ext"]);
    ready[9] = 4; // max_msgs_per_htc_bundle = 4, as the oracle sets
    for (n, v) in vectors.iter().enumerate() {
        let n = n + 1;
        let line = lines_with(HTC, &format!("trailer.{n} ")).pop().unwrap();
        let f: Vec<&str> = line.split(' ').collect();
        let status: i32 = f[1]["status=".len()..].parse().unwrap();
        let la_len: usize = f[2]["la_len=".len()..].parse().unwrap();
        let la = &f[3]["la=".len()..];
        let credits: Vec<u32> = f[4]["credits=".len()..]
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect();

        let mut htc = Htc::new().unwrap();
        htc.on_control_message(&ready).unwrap();
        assert_eq!(htc.max_msgs_per_htc_bundle(), 4);
        let r = htc.process_trailer(v);
        if n == 11 {
            // the C accepts a record that claims four bytes beyond the trailer (and reads them); we do not
            assert_eq!(status, 0);
            assert_eq!(r, Err(htc::HtcError::Invalid));
            continue;
        }
        match r {
            Ok(l) => {
                assert_eq!(status, 0, "vector {n}");
                assert_eq!(l.entries.len(), la_len, "vector {n}");
                let hexs: String = l.entries.iter().map(|e| to_hex(e)).collect();
                assert_eq!(hexs, la, "vector {n}");
            }
            Err(e) => {
                assert_eq!(status, -22, "vector {n}");
                assert_eq!(e, htc::HtcError::Invalid);
            }
        }
        if status == 0 {
            let got: Vec<u32> = (0..9)
                .map(|i| htc.endpoint(i).unwrap().tx_credits)
                .collect();
            assert_eq!(got, credits, "vector {n}");
        }
    }
}

#[test]
fn pipe_configs_serialise() {
    assert_eq!(
        to_hex(&TARGET_CE_CONFIG[4].to_le_bytes()),
        "040000000200000000010000000100000800000000000000"
    );
    assert_eq!(
        to_hex(&snoc::TARGET_SERVICE_TO_CE_MAP[0].to_le_bytes()),
        "040100000200000003000000"
    );
    assert_eq!(
        to_hex(&snoc::TARGET_SHADOW_REG_CFG[5].to_le_bytes()),
        "01004000"
    );
}
