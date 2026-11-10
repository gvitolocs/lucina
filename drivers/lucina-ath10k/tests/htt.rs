//! HTT framing against oracle/htt_oracle.c (real structs from htt.h, real wcn3990 rx descriptor offsets).
//! The expected values derive from Linux ath10k (SPDX ISC), see oracle/README.md.

mod common;

use common::{hex, kvl, num, to_hex};
use lucina_ath10k::htt::*;

const FIX: &str = include_str!("fixtures/htt_oracle.txt");

fn bytes(name: &str) -> Vec<u8> {
    let l = FIX
        .lines()
        .find(|l| l.starts_with(&format!("bytes.{name} ")))
        .unwrap_or_else(|| panic!("{name}"));
    hex(l.split(' ').nth(1).unwrap())
}

#[test]
fn layout_and_constants_match_htt_h() {
    let k = kvl(FIX);
    let g = |n: &str| num(&k[n]);
    assert_eq!(g("sizeof.struct htt_cmd_hdr"), 1);
    assert_eq!(g("sizeof.struct htt_ver_req"), 3);
    assert_eq!(g("sizeof.struct htt_resp_hdr"), 1);
    assert_eq!(g("sizeof.struct htt_ver_resp"), 3);
    assert_eq!(g("offsetof.struct htt_ver_resp.minor"), 0);
    assert_eq!(g("offsetof.struct htt_ver_resp.major"), 1);
    assert_eq!(g("sizeof.struct htt_rx_ring_rx_desc_offsets"), 20);
    assert_eq!(g("sizeof.struct htt_rx_ring_setup_ring64"), 44);
    assert_eq!(g("sizeof.struct htt_rx_ring_setup_ring32"), 36);
    assert_eq!(g("sizeof.struct htt_rx_ring_setup_hdr"), 3);
    assert_eq!(
        g("offsetof.struct htt_rx_ring_setup_ring64.rx_ring_base_paddr"),
        8
    );
    assert_eq!(
        g("offsetof.struct htt_rx_ring_setup_ring64.rx_ring_len"),
        16
    );
    assert_eq!(
        g("offsetof.struct htt_rx_ring_setup_ring64.rx_ring_bufsize"),
        18
    );
    assert_eq!(g("offsetof.struct htt_rx_ring_setup_ring64.flags"), 20);
    assert_eq!(
        g("offsetof.struct htt_rx_ring_setup_ring64.fw_idx_init_val"),
        22
    );
    assert_eq!(g("offsetof.struct htt_rx_ring_setup_ring64.offsets"), 24);
    assert_eq!(g("offsetof.struct htt_rx_ring_setup_ring32.offsets"), 16);
    assert_eq!(g("offsetof.struct htt_rx_ring_setup_64.rings"), 3);
    assert_eq!(
        g("sizeof.struct htt_data_tx_desc_64") as usize,
        DATA_TX_DESC_64_LEN
    );
    assert_eq!(g("sizeof.struct htt_data_tx_desc"), 15);
    for (f, off) in [
        ("flags0", 0),
        ("flags1", 1),
        ("len", 3),
        ("id", 5),
        ("frags_paddr", 7),
        ("peerid", 15),
        ("offchan_tx.freq", 17),
        ("prefetch", 19),
    ] {
        assert_eq!(
            g(&format!("offsetof.struct htt_data_tx_desc_64.{f}")),
            off,
            "{f}"
        );
    }
    assert_eq!(g("sizeof.struct htt_data_tx_desc_frag"), 8);
    assert_eq!(
        g("offsetof.struct htt_data_tx_desc_frag.tword_addr.paddr_hi"),
        4
    );
    assert_eq!(
        g("offsetof.struct htt_data_tx_desc_frag.tword_addr.len_16"),
        6
    );
    assert_eq!(
        g("sizeof.struct htt_msdu_ext_desc_64") as usize,
        MSDU_EXT_DESC_64_LEN
    );
    assert_eq!(
        g("offsetof.struct htt_msdu_ext_desc_64.ip_identification"),
        20
    );
    assert_eq!(g("offsetof.struct htt_msdu_ext_desc_64.flags"), 22);
    assert_eq!(g("offsetof.struct htt_msdu_ext_desc_64.frags"), 24);
    assert_eq!(g("sizeof.struct ath10k_htt_txbuf_64"), 44);
    assert_eq!(g("offsetof.struct ath10k_htt_txbuf_64.htc_hdr"), 16);
    assert_eq!(g("offsetof.struct ath10k_htt_txbuf_64.cmd_hdr"), 24);
    assert_eq!(g("offsetof.struct ath10k_htt_txbuf_64.cmd_tx"), 25);
    assert_eq!(g("sizeof.struct htt_data_tx_completion"), 3);
    assert_eq!(g("offsetof.struct htt_data_tx_completion.num_msdus"), 1);
    assert_eq!(g("offsetof.struct htt_data_tx_completion.flags2"), 2);
    assert_eq!(g("offsetof.struct htt_data_tx_completion.msdus"), 3);
    assert_eq!(
        g("sizeof.struct htt_rx_offload_msdu") as usize,
        RX_OFFLOAD_MSDU_HDR_LEN
    );
    assert_eq!(g("offsetof.struct htt_rx_offload_msdu.peer_id"), 2);
    assert_eq!(g("offsetof.struct htt_rx_offload_msdu.vdev_id"), 4);
    assert_eq!(g("offsetof.struct htt_rx_offload_msdu.tid"), 5);
    assert_eq!(g("offsetof.struct htt_rx_offload_msdu.fw_desc"), 6);
    assert_eq!(g("sizeof.struct htt_rx_offload_ind"), 3);
    assert_eq!(g("offsetof.struct htt_rx_offload_ind.msdu_count"), 1);
    assert_eq!(g("sizeof.struct htt_rx_in_ord_msdu_desc"), 8);
    assert_eq!(
        g("sizeof.struct htt_rx_in_ord_msdu_desc_ext") as usize,
        IN_ORD_MSDU_DESC_EXT_LEN
    );
    assert_eq!(g("sizeof.struct htt_rx_in_ord_ind"), 7);
    assert_eq!(g("offsetof.struct htt_rx_in_ord_ind.peer_id"), 1);
    assert_eq!(g("offsetof.struct htt_rx_in_ord_ind.vdev_id"), 3);
    assert_eq!(g("offsetof.struct htt_rx_in_ord_ind.msdu_count"), 5);
    assert_eq!(g("offsetof.struct htt_rx_in_ord_ind.msdu_descs64"), 7);
    assert_eq!(
        g("offsetof.struct htt_resp.rx_in_ord_ind") as usize + 7,
        IN_ORD_IND_HDR_LEN
    );
    assert_eq!(g("sizeof.struct htt_data_tx_ppdu_dur"), 8);

    let consts: &[(&str, u64)] = &[
        ("HTT_H2T_MSG_TYPE_VERSION_REQ", u64::from(h2t::VERSION_REQ)),
        ("HTT_H2T_MSG_TYPE_TX_FRM", u64::from(h2t::TX_FRM)),
        ("HTT_H2T_MSG_TYPE_RX_RING_CFG", u64::from(h2t::RX_RING_CFG)),
        ("HTT_H2T_MSG_TYPE_STATS_REQ", u64::from(h2t::STATS_REQ)),
        ("HTT_H2T_MSG_TYPE_SYNC", u64::from(h2t::SYNC)),
        ("HTT_H2T_MSG_TYPE_AGGR_CFG", u64::from(h2t::AGGR_CFG)),
        (
            "HTT_H2T_MSG_TYPE_FRAG_DESC_BANK_CFG",
            u64::from(h2t::FRAG_DESC_BANK_CFG),
        ),
        ("HTT_H2T_MSG_TYPE_MGMT_TX", u64::from(h2t::MGMT_TX)),
        (
            "HTT_H2T_MSG_TYPE_TX_FETCH_RESP",
            u64::from(h2t::TX_FETCH_RESP),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_VERSION_CONF",
            u64::from(t2h::VERSION_CONF),
        ),
        ("HTT_TLV_T2H_MSG_TYPE_RX_IND", u64::from(t2h::RX_IND)),
        ("HTT_TLV_T2H_MSG_TYPE_RX_FLUSH", u64::from(t2h::RX_FLUSH)),
        ("HTT_TLV_T2H_MSG_TYPE_PEER_MAP", u64::from(t2h::PEER_MAP)),
        (
            "HTT_TLV_T2H_MSG_TYPE_PEER_UNMAP",
            u64::from(t2h::PEER_UNMAP),
        ),
        ("HTT_TLV_T2H_MSG_TYPE_RX_ADDBA", u64::from(t2h::RX_ADDBA)),
        ("HTT_TLV_T2H_MSG_TYPE_RX_DELBA", u64::from(t2h::RX_DELBA)),
        (
            "HTT_TLV_T2H_MSG_TYPE_TX_COMPL_IND",
            u64::from(t2h::TX_COMPL_IND),
        ),
        ("HTT_TLV_T2H_MSG_TYPE_PKTLOG", u64::from(t2h::PKTLOG)),
        (
            "HTT_TLV_T2H_MSG_TYPE_STATS_CONF",
            u64::from(t2h::STATS_CONF),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_RX_FRAG_IND",
            u64::from(t2h::RX_FRAG_IND),
        ),
        ("HTT_TLV_T2H_MSG_TYPE_SEC_IND", u64::from(t2h::SEC_IND)),
        (
            "HTT_TLV_T2H_MSG_TYPE_RC_UPDATE_IND",
            u64::from(t2h::RC_UPDATE_IND),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_TX_INSPECT_IND",
            u64::from(t2h::TX_INSPECT_IND),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_MGMT_TX_COMPL_IND",
            u64::from(t2h::MGMT_TX_COMPL_IND),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_TX_CREDIT_UPDATE_IND",
            u64::from(t2h::TX_CREDIT_UPDATE_IND),
        ),
        ("HTT_TLV_T2H_MSG_TYPE_RX_PN_IND", u64::from(t2h::RX_PN_IND)),
        (
            "HTT_TLV_T2H_MSG_TYPE_RX_OFFLOAD_DELIVER_IND",
            u64::from(t2h::RX_OFFLOAD_DELIVER_IND),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_RX_IN_ORD_PADDR_IND",
            u64::from(t2h::RX_IN_ORD_PADDR_IND),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_WDI_IPA_OP_RESPONSE",
            u64::from(t2h::WDI_IPA_OP_RESPONSE),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_CHAN_CHANGE",
            u64::from(t2h::CHAN_CHANGE),
        ),
        (
            "HTT_TLV_T2H_MSG_TYPE_RX_OFLD_PKT_ERR",
            u64::from(t2h::RX_OFLD_PKT_ERR),
        ),
        ("HTT_TLV_T2H_MSG_TYPE_TEST", u64::from(t2h::TEST)),
        ("HTT_TLV_T2H_NUM_MSGS", u64::from(t2h::NUM_MSGS)),
        (
            "HTT_DATA_TX_DESC_FLAGS0_MAC_HDR_PRESENT",
            u64::from(tx_flags0::MAC_HDR_PRESENT),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS0_NO_AGGR",
            u64::from(tx_flags0::NO_AGGR),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS0_NO_ENCRYPT",
            u64::from(tx_flags0::NO_ENCRYPT),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS0_NO_CLASSIFY",
            u64::from(tx_flags0::NO_CLASSIFY),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS0_PKT_TYPE_MASK",
            u64::from(tx_flags0::PKT_TYPE_MASK),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS0_PKT_TYPE_LSB",
            u64::from(tx_flags0::PKT_TYPE_LSB),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_VDEV_ID_MASK",
            u64::from(tx_flags1::VDEV_ID_MASK),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_VDEV_ID_LSB",
            u64::from(tx_flags1::VDEV_ID_LSB),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_EXT_TID_MASK",
            u64::from(tx_flags1::EXT_TID_MASK),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_EXT_TID_LSB",
            u64::from(tx_flags1::EXT_TID_LSB),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_POSTPONED",
            u64::from(tx_flags1::POSTPONED),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_MORE_IN_BATCH",
            u64::from(tx_flags1::MORE_IN_BATCH),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_CKSUM_L3_OFFLOAD",
            u64::from(tx_flags1::CKSUM_L3_OFFLOAD),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_CKSUM_L4_OFFLOAD",
            u64::from(tx_flags1::CKSUM_L4_OFFLOAD),
        ),
        (
            "HTT_DATA_TX_DESC_FLAGS1_TX_COMPLETE",
            u64::from(tx_flags1::TX_COMPLETE),
        ),
        (
            "HTT_DATA_TX_EXT_TID_NON_QOS_MCAST_BCAST",
            u64::from(ext_tid::NON_QOS_MCAST_BCAST),
        ),
        ("HTT_DATA_TX_EXT_TID_MGMT", u64::from(ext_tid::MGMT)),
        ("HTT_DATA_TX_EXT_TID_INVALID", u64::from(ext_tid::INVALID)),
        ("HTT_INVALID_PEERID", u64::from(INVALID_PEERID)),
        (
            "HTT_MSDU_CHECKSUM_ENABLE_64",
            u64::from(MSDU_CHECKSUM_ENABLE_64),
        ),
        (
            "HTT_RX_RING_FLAGS_MAC80211_HDR",
            u64::from(rx_ring_flags::MAC80211_HDR),
        ),
        (
            "HTT_RX_RING_FLAGS_PHY_DATA_RX",
            u64::from(rx_ring_flags::PHY_DATA_RX),
        ),
        ("HTT_RX_RING_SIZE", u64::from(RX_RING_SIZE)),
        (
            "HTT_RX_RING_FILL_LEVEL_DUAL_MAC",
            u64::from(RX_RING_FILL_LEVEL),
        ),
        ("HTT_DATA_TX_STATUS_OK", u64::from(tx_status::OK)),
        ("HTT_DATA_TX_STATUS_DISCARD", u64::from(tx_status::DISCARD)),
        ("HTT_DATA_TX_STATUS_NO_ACK", u64::from(tx_status::NO_ACK)),
        (
            "HTT_DATA_TX_STATUS_POSTPONE",
            u64::from(tx_status::POSTPONE),
        ),
        ("HTT_DATA_TX_STATUS_MASK", 7),
        ("HTT_DATA_TX_TID_MASK", 0x78),
        ("HTT_DATA_TX_TID_LSB", 3),
        ("HTT_DATA_TX_TID_INVALID", 0x80),
        ("HTT_TX_COMPL_INV_MSDU_ID", u64::from(TX_COMPL_INV_MSDU_ID)),
        (
            "HTT_TX_CMPL_FLAG_DATA_RSSI",
            u64::from(tx_compl_flags2::DATA_RSSI),
        ),
        (
            "HTT_TX_CMPL_FLAG_PPID_PRESENT",
            u64::from(tx_compl_flags2::PPID_PRESENT),
        ),
        (
            "HTT_TX_CMPL_FLAG_PA_PRESENT",
            u64::from(tx_compl_flags2::PA_PRESENT),
        ),
        (
            "HTT_TX_CMPL_FLAG_PPDU_DURATION_PRESENT",
            u64::from(tx_compl_flags2::PPDU_DURATION_PRESENT),
        ),
        (
            "HTT_TX_DATA_RSSI_ENABLE_WCN3990",
            u64::from(tx_compl_flags2::RSSI_ENABLE_WCN3990),
        ),
        (
            "HTT_TX_DATA_APPEND_RETRIES",
            u64::from(tx_compl_flags2::APPEND_RETRIES),
        ),
        (
            "HTT_TX_DATA_APPEND_TIMESTAMP",
            u64::from(tx_compl_flags2::APPEND_TIMESTAMP),
        ),
        (
            "HTT_RX_IN_ORD_IND_INFO_TID_MASK",
            u64::from(in_ord_info::TID_MASK),
        ),
        (
            "HTT_RX_IN_ORD_IND_INFO_OFFLOAD_MASK",
            u64::from(in_ord_info::OFFLOAD),
        ),
        (
            "HTT_RX_IN_ORD_IND_INFO_FRAG_MASK",
            u64::from(in_ord_info::FRAG),
        ),
        ("ATH10K_HW_TXRX_RAW", u64::from(txrx_mode::RAW)),
        (
            "ATH10K_HW_TXRX_NATIVE_WIFI",
            u64::from(txrx_mode::NATIVE_WIFI),
        ),
        ("ATH10K_HW_TXRX_ETHERNET", u64::from(txrx_mode::ETHERNET)),
        ("ATH10K_HW_TXRX_MGMT", u64::from(txrx_mode::MGMT)),
    ];
    for (n, want) in consts {
        assert_eq!(g(&format!("const.{n}")), *want, "{n}");
    }
    assert_eq!(g("const.HTT_RX_RING_SIZE_MAX"), u64::from(RX_RING_SIZE));
    // WCN3990 rx descriptor
    assert_eq!(
        to_hex(&WCN3990_RX_DESC_OFFSETS.to_le_bytes()),
        to_hex(&bytes("wcn3990_rx_desc_offsets"))
    );
    assert_eq!(g("wcn3990_rx_desc.size") as usize, RX_DESC_SIZE);
    assert_eq!(
        g("wcn3990_rx_desc.msdu_payload_offset") as usize,
        RX_DESC_SIZE
    );
    assert_eq!(g("wcn3990_rx_desc.attention"), 4);
    assert_eq!(g("wcn3990_rx_desc.rx_hdr_status"), 296);
    assert_eq!(
        g("sizeof.struct ath10k_htt_txbuf_64") as usize,
        16 + crate_htc_hdr() + 1 + DATA_TX_DESC_64_LEN
    );
}

fn crate_htc_hdr() -> usize {
    lucina_ath10k::htc::HDR_LEN
}

#[test]
fn host_to_target_messages_match_the_c() {
    assert_eq!(ver_req().to_vec(), bytes("ver_req"));
    let r = RxRingSetup::wcn3990(0x1_2345_6780, 0x1_fedc_ba00, 0x1234);
    assert_eq!(
        to_hex(&rx_ring_setup_64(&r)),
        to_hex(&bytes("rx_ring_cfg_64"))
    );
    let c = lucina_ath10k::htt::RxRingSetup {
        fw_idx_shadow_reg_paddr: 0x2345_6780,
        rx_ring_base_paddr: 0xfedc_ba00,
        rx_ring_len: 0x400,
        rx_ring_bufsize: 2048,
        flags: 0xa5a5,
        fw_idx_init_val: 0x1234,
        offsets: RxDescOffsets {
            mac80211_hdr_offset: 1,
            msdu_payload_offset: 2,
            ppdu_start_offset: 3,
            ppdu_end_offset: 4,
            mpdu_start_offset: 5,
            mpdu_end_offset: 6,
            msdu_start_offset: 7,
            msdu_end_offset: 8,
            rx_attention_offset: 9,
            frag_info_offset: 10,
        },
    };
    assert_eq!(
        to_hex(&rx_ring_setup_32(&c).unwrap()),
        to_hex(&bytes("rx_ring_cfg_32"))
    );
    assert_eq!(
        rx_ring_setup_32(&RxRingSetup {
            rx_ring_base_paddr: 1 << 32,
            ..c
        }),
        Err(HttError::AddressTooWide)
    );
    let line = FIX
        .lines()
        .find(|l| l.starts_with("rx_ring_cfg.flags_all "))
        .unwrap();
    assert_eq!(
        num(line.split(' ').nth(1).unwrap()) as u16,
        rx_ring_flags::ALL
    );

    // tx frames
    let cases = [
        (
            tx_flags0_for(txrx_mode::NATIVE_WIFI, false),
            tx_flags1_for(1, 5, false),
            100u16,
            0x2au16,
            0x7_f000_0000u64,
            None,
            32usize,
        ),
        (
            tx_flags0_for(txrx_mode::NATIVE_WIFI, false),
            tx_flags1_for(2, 5, true),
            101,
            0x2b,
            0x7_f000_0100,
            Some(0),
            32,
        ),
        (
            tx_flags0_for(txrx_mode::MGMT, true),
            tx_flags1_for(3, ext_tid::MGMT, false),
            102,
            0x2c,
            0x7_f000_0200,
            Some(5180),
            36,
        ),
    ];
    for (i, (flags0, flags1, len, id, frags_paddr, freq, prefetch)) in cases.into_iter().enumerate()
    {
        let d = DataTxDesc64 {
            flags0,
            flags1,
            len,
            id,
            frags_paddr,
            freq,
        };
        let pre = alloc_prefetch(prefetch);
        let frame = tx_frm_64(1, &d, &pre);
        let want = bytes(&format!("tx_frm_64_{i}"));
        assert_eq!(to_hex(&frame[..want.len()]), to_hex(&want), "case {i}");
        assert_eq!(frame.len(), want.len() + prefetch);
    }
    // an odd prefetch length is padded to 4 and covered by the HTC length
    let d = DataTxDesc64 {
        flags0: 0,
        flags1: 0,
        len: 1,
        id: 1,
        frags_paddr: 0,
        freq: None,
    };
    let f = tx_frm_64(1, &d, &[9, 9, 9]);
    assert_eq!(f.len(), 8 + 1 + 19 + 4);
    assert_eq!(u16::from_le_bytes([f[2], f[3]]), 1 + 19 + 4);
    assert_eq!(&f[28..], &[9, 9, 9, 0]);

    let e = MsduExtDesc64 {
        paddr: 0x1_2345_6780,
        len: 1500,
        checksum_offload: false,
    };
    assert_eq!(to_hex(&e.to_le_bytes()), to_hex(&bytes("msdu_ext_desc_64")));
    let e = MsduExtDesc64 {
        checksum_offload: true,
        ..e
    };
    assert_eq!(
        to_hex(&e.to_le_bytes()),
        to_hex(&bytes("msdu_ext_desc_64_csum"))
    );
}

fn alloc_prefetch(n: usize) -> Vec<u8> {
    vec![0xee; n]
}

#[test]
fn target_to_host_parsers_match_the_c() {
    // version confirmation
    let b = bytes("ver_conf");
    let k = kvl(FIX);
    assert_eq!(
        parse_version_conf(&b),
        Ok((
            num(&k["ver_conf.major"]) as u8,
            num(&k["ver_conf.minor"]) as u8
        ))
    );
    assert_eq!(parse_version_conf(&b[..4]), Ok((2, 3)));
    assert_eq!(parse_version_conf(&b[..3]), Err(HttError::Short));
    assert_eq!(
        parse_version_conf(&[t2h::RX_IND, 0, 0, 0]),
        Err(HttError::WrongType)
    );
    assert_eq!(parse_resp(&b).unwrap().0, T2h::VersionConf);
    assert_eq!(parse_resp(&[]), Err(HttError::Short));
    assert_eq!(parse_resp(&[0x13]).unwrap().0, T2h::Unsupported(0x13));
    assert_eq!(parse_resp(&[0x18]).unwrap().0, T2h::Unsupported(0x18));
    assert_eq!(parse_resp(&[0x12]).unwrap().0, T2h::RxInOrdPaddrInd);

    // credit updates
    for line in FIX.lines().filter(|l| l.starts_with("credit_update.")) {
        let f: Vec<&str> = line.split(' ').collect();
        let word = num(&f[1]["word=".len()..]) as u32;
        let delta: i32 = f[2]["delta=".len()..].parse().unwrap();
        assert_eq!(
            parse_tx_credit_update(&word.to_le_bytes()),
            Ok(delta),
            "{line}"
        );
    }

    // tx completions
    for name in [
        "tx_compl_even",
        "tx_compl_odd",
        "tx_compl_rssi_even",
        "tx_compl_rssi_odd",
        "tx_compl_rssi_retries_ts",
    ] {
        let b = bytes(name);
        let tc = parse_tx_completion(&b).unwrap();
        let want: Vec<&str> = FIX
            .lines()
            .filter(|l| l.starts_with(&format!("{name}.msdu.")))
            .collect();
        assert_eq!(tc.msdus.len(), want.len(), "{name}");
        for (m, l) in tc.msdus.iter().zip(want) {
            let id = num(l.split("id=").nth(1).unwrap().split(' ').next().unwrap()) as u16;
            let rssi = num(l.split("ack_rssi=").nth(1).unwrap()) as u16;
            assert_eq!(m.msdu_id, id, "{l}");
            assert_eq!(m.ack_rssi.unwrap_or(0), rssi, "{l}");
            assert_eq!(m.ack_rssi.is_some(), name.contains("rssi"), "{l}");
        }
        assert_eq!(tc.tid, 5);
        assert!(!tc.tid_invalid);
        assert_eq!(
            tc.result,
            if name == "tx_compl_odd" {
                TxResult::NoAck
            } else {
                TxResult::Ack
            },
            "{name}"
        );
        // every proper prefix that cuts a word the reader needs is refused, never panics
        for cut in 0..b.len() {
            let r = parse_tx_completion(&b[..cut]);
            if let Ok(t) = r {
                assert!(t.msdus.len() <= usize::from(b[2]));
            }
        }
    }
    assert_eq!(
        parse_tx_completion(&bytes("tx_compl_rssi_even")[..18]),
        Err(HttError::Truncated)
    );
    assert_eq!(
        parse_tx_completion(&[t2h::RX_IND, 0, 0, 0]),
        Err(HttError::WrongType)
    );
    // statuses
    for (st, want) in [
        (0u8, TxResult::Ack),
        (1, TxResult::Discard),
        (2, TxResult::NoAck),
        (3, TxResult::Discard),
        (7, TxResult::Discard),
    ] {
        assert_eq!(
            parse_tx_completion(&[t2h::TX_COMPL_IND, st | 0x80, 0, 0])
                .unwrap()
                .result,
            want
        );
    }
    assert!(
        parse_tx_completion(&[t2h::TX_COMPL_IND, 0x80, 0, 0])
            .unwrap()
            .tid_invalid
    );

    // in-order indication, 64-bit descriptors
    let b = bytes("rx_in_ord_ind_2");
    let ind = parse_rx_in_ord_ind(&b).unwrap();
    assert_eq!(
        (ind.tid, ind.offload, ind.frag, ind.peer_id, ind.vdev_id),
        (5, false, true, 0x1234, 2)
    );
    assert_eq!(
        ind.msdus,
        vec![
            InOrdMsdu {
                paddr: 0x1_dead_be00,
                msdu_len: 1500,
                fw_desc: 0x11
            },
            InOrdMsdu {
                paddr: 0x2_cafe_f000,
                msdu_len: 60,
                fw_desc: 0x22
            }
        ]
    );
    assert_eq!(
        num(&k["rx_in_ord_ind_2.header_len"]) as usize,
        IN_ORD_IND_HDR_LEN
    );
    let ind = parse_rx_in_ord_ind(&bytes("rx_in_ord_ind_2_offload")).unwrap();
    assert_eq!((ind.tid, ind.offload, ind.frag), (3, true, false));
    // the C's bound (8 bytes per entry) would accept this; 12 bytes per entry is what the list needs
    assert_eq!(
        parse_rx_in_ord_ind(&b[..b.len() - 4]),
        Err(HttError::Truncated)
    );
    assert_eq!(parse_rx_in_ord_ind(&b[..7]), Err(HttError::Short));
    // a count that promises 65535 entries
    let mut huge = b[..IN_ORD_IND_HDR_LEN].to_vec();
    huge[6] = 0xff;
    huge[7] = 0xff;
    assert_eq!(parse_rx_in_ord_ind(&huge), Err(HttError::Truncated));

    // offload
    let b = bytes("rx_offload_msdu");
    let m = parse_rx_offload_msdu(&b).unwrap();
    assert_eq!(
        (m.peer_id, m.vdev_id, m.tid, m.fw_desc, m.payload),
        (0x77, 1, 6, 0x40, &b"0123456789ab"[..])
    );
    assert_eq!(
        parse_rx_offload_msdu(&b[..b.len() - 1]),
        Err(HttError::Truncated)
    );
    assert_eq!(parse_rx_offload_msdu(&b[..6]), Err(HttError::Short));
    assert_eq!(
        parse_rx_offload_ind(&[t2h::RX_OFFLOAD_DELIVER_IND, 0, 5, 0]),
        Ok(5)
    );
    assert_eq!(
        parse_rx_offload_ind(&[t2h::RX_IND, 0, 5, 0]),
        Err(HttError::WrongType)
    );
}

#[test]
fn driver_messages_match_the_c_layout() {
    let k = kvl(FIX);
    let g = |n: &str| num(&k[n]);
    assert_eq!(
        g("sizeof.struct htt_frag_desc_bank_cfg64") as usize,
        FRAG_DESC_BANK_CFG_64_LEN
    );
    assert_eq!(
        g("const.HTT_H2T_MSG_TYPE_FRAG_DESC_BANK_CFG") as u8,
        h2t::FRAG_DESC_BANK_CFG
    );
    assert_eq!(g("const.HTT_H2T_MSG_TYPE_AGGR_CFG") as u8, h2t::AGGR_CFG);
    assert_eq!(
        g("const.ATH10K_HTT_MAX_NUM_AMPDU_DEFAULT") as u8,
        MAX_NUM_AMPDU_DEFAULT
    );
    assert_eq!(
        g("const.ATH10K_HTT_MAX_NUM_AMSDU_DEFAULT") as u8,
        MAX_NUM_AMSDU_DEFAULT
    );
    assert_eq!(
        to_hex(&frag_desc_bank_cfg_64(0x1_2345_6700, 127)),
        to_hex(&bytes("frag_desc_bank_cfg_64"))
    );
    assert_eq!(
        to_hex(&aggr_cfg_v2(64, 3).unwrap()),
        to_hex(&bytes("aggr_cfg_v2"))
    );
    assert_eq!(aggr_cfg_v2(0, 3), None);
    assert_eq!(aggr_cfg_v2(65, 3), None);
    assert_eq!(aggr_cfg_v2(64, 0), None);
    assert_eq!(aggr_cfg_v2(64, 32), None);
    assert_eq!(
        parse_peer_map(&bytes("peer_map")).unwrap(),
        (2, 0x123, [0xde, 0xad, 0xbe, 0xef, 0, 1])
    );
    assert_eq!(parse_peer_unmap(&bytes("peer_unmap")).unwrap(), 0x123);
    assert_eq!(
        parse_mgmt_tx_compl(&bytes("mgmt_tx_compl")).unwrap(),
        (9, mgmt_tx_status::RETRY)
    );
    assert_eq!(parse_sec_ind(&bytes("sec_ind")).unwrap(), (true, 6, 0x45));
    let r = parse_rx_ind(&bytes("rx_ind_2ranges")).unwrap();
    assert_eq!((r.tid, r.peer_id, r.mpdu_count), (5, 0x77, 253));
    assert_eq!(
        num(&FIX
            .lines()
            .find(|l| l.starts_with("rx_ind.ranges_off"))
            .unwrap()[18..]),
        56
    );
    assert_eq!(
        g("sizeof.struct ath10k_htt_txbuf_64") as usize,
        TXBUF_64_LEN
    );
    assert_eq!(
        g("offsetof.struct ath10k_htt_txbuf_64.htc_hdr") as usize,
        TXBUF_64_HTC_HDR_OFF
    );
    for (name, v) in [
        ("rxd.attention", rx_desc::ATTENTION),
        (
            "rxd.frag_info.common.ring2_more_count",
            rx_desc::RING2_MORE_COUNT,
        ),
        ("rxd.mpdu_start.info0", rx_desc::MPDU_START_INFO0),
        ("rxd.mpdu_start.pn", rx_desc::MPDU_START_PN),
        ("rxd.mpdu_start.info1", rx_desc::MPDU_START_INFO1),
        ("rxd.msdu_start.common.info0", rx_desc::MSDU_START_INFO0),
        ("rxd.msdu_start.common.info1", rx_desc::MSDU_START_INFO1),
        ("rxd.msdu_end.common.info0", rx_desc::MSDU_END_INFO0),
        ("rxd.ppdu_start.rssi_chains", rx_desc::PPDU_RSSI_CHAINS),
        ("rxd.ppdu_start.rssi_comb", rx_desc::PPDU_RSSI_COMB),
        ("rxd.ppdu_start.info1", rx_desc::PPDU_INFO1),
        ("rxd.ppdu_start.info2", rx_desc::PPDU_INFO2),
        ("rxd.ppdu_start.info3", rx_desc::PPDU_INFO3),
        ("rxd.rx_hdr_status", rx_desc::HDR_STATUS),
        ("rxd.msdu_payload", RX_DESC_SIZE),
    ] {
        assert_eq!(g(name) as usize, v, "{name}");
    }
    for (name, v) in [
        (
            "const.RX_ATTENTION_FLAGS_FIRST_MPDU",
            rx_desc::ATT_FIRST_MPDU,
        ),
        ("const.RX_ATTENTION_FLAGS_LAST_MPDU", rx_desc::ATT_LAST_MPDU),
        (
            "const.RX_ATTENTION_FLAGS_MCAST_BCAST",
            rx_desc::ATT_MCAST_BCAST,
        ),
        (
            "const.RX_ATTENTION_FLAGS_PEER_IDX_INVALID",
            rx_desc::ATT_PEER_IDX_INVALID,
        ),
        ("const.RX_ATTENTION_FLAGS_MGMT_TYPE", rx_desc::ATT_MGMT_TYPE),
        (
            "const.RX_ATTENTION_FLAGS_MSDU_LENGTH_ERR",
            rx_desc::ATT_MSDU_LENGTH_ERR,
        ),
        (
            "const.RX_ATTENTION_FLAGS_MPDU_LENGTH_ERR",
            rx_desc::ATT_MPDU_LENGTH_ERR,
        ),
        (
            "const.RX_ATTENTION_FLAGS_TKIP_MIC_ERR",
            rx_desc::ATT_TKIP_MIC_ERR,
        ),
        (
            "const.RX_ATTENTION_FLAGS_DECRYPT_ERR",
            rx_desc::ATT_DECRYPT_ERR,
        ),
        ("const.RX_ATTENTION_FLAGS_FCS_ERR", rx_desc::ATT_FCS_ERR),
        ("const.RX_ATTENTION_FLAGS_MSDU_DONE", rx_desc::ATT_MSDU_DONE),
        (
            "const.RX_MSDU_END_INFO0_FIRST_MSDU",
            rx_desc::MSDU_END_FIRST,
        ),
        ("const.RX_MSDU_END_INFO0_LAST_MSDU", rx_desc::MSDU_END_LAST),
        (
            "const.RX_MSDU_START_INFO0_MSDU_LENGTH_MASK",
            rx_desc::MSDU_LENGTH_MASK,
        ),
        ("const.HTT_RX_MPDU_ENCRYPT_NONE", rx_desc::ENCRYPT_NONE),
        (
            "const.HTT_RX_MPDU_ENCRYPT_AES_CCM_WPA2",
            rx_desc::ENCRYPT_AES_CCM_WPA2,
        ),
        (
            "const.HTT_RX_MPDU_ENCRYPT_TKIP_WPA",
            rx_desc::ENCRYPT_TKIP_WPA,
        ),
        ("const.RX_MSDU_DECAP_RAW", rx_desc::DECAP_RAW),
        (
            "const.RX_MSDU_DECAP_NATIVE_WIFI",
            rx_desc::DECAP_NATIVE_WIFI,
        ),
        (
            "const.RX_MSDU_DECAP_ETHERNET2_DIX",
            rx_desc::DECAP_ETHERNET2,
        ),
        (
            "const.RX_MSDU_DECAP_8023_SNAP_LLC",
            rx_desc::DECAP_8023_SNAP,
        ),
        ("const.HTT_RX_LEGACY", rx_desc::PREAMBLE_LEGACY),
        ("const.HTT_RX_HT", rx_desc::PREAMBLE_HT),
        ("const.HTT_RX_HT_WITH_TXBF", rx_desc::PREAMBLE_HT_TXBF),
        ("const.HTT_RX_VHT", rx_desc::PREAMBLE_VHT),
        ("const.HTT_RX_VHT_WITH_TXBF", rx_desc::PREAMBLE_VHT_TXBF),
    ] {
        assert_eq!(g(name) as u32, v, "{name}");
    }
}

#[test]
fn tx_item_is_the_head_of_the_full_tx_frame() {
    let desc = DataTxDesc64 {
        flags0: tx_flags0_for(txrx_mode::NATIVE_WIFI, false),
        flags1: tx_flags1_for(1, 0, false),
        len: 100,
        id: 3,
        frags_paddr: 0x1000,
        freq: None,
    };
    let whole = tx_frm_64(2, &desc, &[0xaa; 52]);
    let item = tx_item_64(2, &desc, 52);
    assert_eq!(&whole[..TXBUF_64_ITEM_LEN], &item[..]);
}

#[test]
fn driver_parsers_refuse_short_and_lying_messages() {
    assert_eq!(parse_peer_map(&[]), Err(HttError::Short));
    assert_eq!(parse_peer_map(&[t2h::PEER_MAP, 1, 2]), Err(HttError::Short));
    assert_eq!(
        parse_peer_unmap(&[t2h::PEER_MAP, 0, 0, 0]),
        Err(HttError::WrongType)
    );
    assert_eq!(
        parse_mgmt_tx_compl(&[t2h::MGMT_TX_COMPL_IND; 11]),
        Err(HttError::Short)
    );
    assert_eq!(parse_sec_ind(&[t2h::SEC_IND, 0, 0]), Err(HttError::Short));
    let mut m = bytes("rx_ind_2ranges");
    // more ranges than bytes
    m[7] = 200;
    assert_eq!(parse_rx_ind(&m), Err(HttError::Truncated));
    m[7] = 2;
    // a firmware descriptor length past the end
    m[44] = 0xff;
    m[45] = 0xff;
    assert_eq!(parse_rx_ind(&m), Err(HttError::Truncated));
    let mut l = common::Lfsr(0xabcd_0001);
    for _ in 0..2000 {
        let n = (l.next() % 80) as usize;
        let mut d = vec![0u8; n];
        l.fill(&mut d);
        if n > 0 {
            d[0] = t2h::RX_IND;
        }
        let _ = parse_rx_ind(&d);
        let _ = parse_peer_map(&d);
    }
}
