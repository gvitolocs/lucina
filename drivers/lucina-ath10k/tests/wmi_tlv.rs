//! WMI-TLV against oracle/wmi_oracle.c: the real encoders and parsers of Linux compiled on the host.
//! The expected values derive from Linux ath10k (SPDX ISC), see oracle/README.md.

mod common;

use common::{hex, kvl, num, to_hex};
use lucina_ath10k::wmi_tlv::*;

const FIX: &str = include_str!("fixtures/wmi_oracle.txt");

fn line_after(prefix: &str) -> &'static str {
    FIX.lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("no fixture line {prefix}"))
}

/// `name <len> <hex>` or `name err <errno>`
fn cmd_line(name: &str) -> Result<Vec<u8>, i32> {
    let l = line_after(&format!("{name} "));
    let mut it = l.split(' ').skip(1);
    let a = it.next().unwrap();
    if a == "err" {
        return Err(it.next().unwrap().parse().unwrap());
    }
    let n: usize = a.parse().unwrap();
    let b = hex(it.next().unwrap_or(""));
    assert_eq!(b.len(), n, "{name}");
    Ok(b)
}

fn check(name: &str, got: Result<Vec<u8>, WmiError>) {
    match (cmd_line(name), got) {
        (Ok(want), Ok(got)) => assert_eq!(to_hex(&got), to_hex(&want), "{name}"),
        (Err(e), Err(g)) => assert_eq!(g.errno(), e, "{name}"),
        (a, b) => panic!("{name}: C {a:?}, Rust {b:?}"),
    }
}

#[test]
fn layout_ids_tags_policy_match_the_headers() {
    let k = kvl(FIX);
    let g = |n: &str| num(&k[n]);
    assert_eq!(g("sizeof.struct wmi_tlv") as usize, TLV_HDR_LEN);
    assert_eq!(g("offsetof.struct wmi_tlv.len"), 0);
    assert_eq!(g("offsetof.struct wmi_tlv.tag"), 2);
    assert_eq!(g("offsetof.struct wmi_tlv.value"), 4);
    assert_eq!(g("sizeof.struct wmi_cmd_hdr") as usize, CMD_HDR_LEN);
    assert_eq!(g("const.WMI_CMD_HDR_CMD_ID_MASK") as u32, CMD_ID_MASK);
    assert_eq!(g("const.WMI_CMD_HDR_PLT_PRIV_MASK"), 0xff00_0000);
    assert_eq!(g("sizeof.struct wmi_mac_addr"), 8);
    assert_eq!(g("sizeof.struct wmi_ssid"), 36);
    assert_eq!(g("sizeof.struct wmi_tlv_svc_rdy_ev"), 104);
    assert_eq!(g("sizeof.struct wmi_tlv_rdy_ev"), 36);
    assert_eq!(g("sizeof.struct hal_reg_capabilities"), 36);
    assert_eq!(g("sizeof.struct wlan_host_mem_req"), 16);
    assert_eq!(g("sizeof.struct wmi_scan_event"), 24);
    assert_eq!(
        g("sizeof.struct wmi_tlv_resource_config") as usize,
        RESOURCE_CONFIG_LEN
    );
    assert_eq!(g("sizeof.struct host_memory_chunk_tlv"), 16);
    assert_eq!(g("sizeof.struct wmi_tlv_init_cmd"), 28);
    assert_eq!(g("sizeof.struct wmi_start_scan_common"), 60);
    assert_eq!(g("sizeof.struct wmi_tlv_start_scan_cmd"), 100);
    assert_eq!(g("sizeof.struct wmi_stop_scan_cmd"), 16);
    assert_eq!(g("sizeof.struct wmi_vdev_create_cmd"), 20);
    assert_eq!(g("sizeof.struct wmi_vdev_up_cmd"), 16);
    assert_eq!(g("sizeof.struct wmi_tlv_peer_create_cmd"), 16);
    assert_eq!(g("sizeof.struct wmi_peer_delete_cmd"), 12);
    assert_eq!(g("sizeof.struct wmi_tlv_pdev_set_rd_cmd"), 24);
    assert_eq!(g("sizeof.struct wmi_tlv_vdev_start_cmd"), 72);
    assert_eq!(g("sizeof.struct wmi_channel"), 24);
    assert_eq!(g("sizeof.struct wmi_tlv_mgmt_tx_cmd"), 28);
    // field offsets the parsers and encoders hard-code
    for (f, off) in [
        ("fw_build_vers", 0),
        ("abi", 4),
        ("phy_capability", 28),
        ("num_rf_chains", 36),
        ("ht_cap_info", 40),
        ("vht_cap_info", 44),
        ("vht_supp_mcs", 48),
        ("hw_min_tx_power", 52),
        ("hw_max_tx_power", 56),
        ("sys_cap_info", 60),
        ("num_mem_reqs", 72),
    ] {
        assert_eq!(
            g(&format!("offsetof.struct wmi_tlv_svc_rdy_ev.{f}")),
            off,
            "{f}"
        );
    }
    assert_eq!(g("offsetof.struct wmi_tlv_rdy_ev.mac_addr"), 24);
    assert_eq!(g("offsetof.struct wmi_tlv_rdy_ev.status"), 32);
    // resource config: every field at the offset its position implies
    let mut word = 0usize;
    for f in RESOURCE_CONFIG_FIELDS {
        assert_eq!(
            g(&format!("offsetof.struct wmi_tlv_resource_config.{f}")) as usize,
            word * 4,
            "{f}"
        );
        word += if f == "rx_timeout_pri" { 4 } else { 1 };
    }
    assert_eq!(word * 4, RESOURCE_CONFIG_LEN);
    for (f, off) in [("req_id", 0), ("ptr", 4), ("size", 8), ("ptr_high", 12)] {
        assert_eq!(
            g(&format!("offsetof.struct host_memory_chunk_tlv.{f}")),
            off,
            "{f}"
        );
    }
    for (f, off) in [
        ("mhz", 0),
        ("band_center_freq1", 4),
        ("band_center_freq2", 8),
        ("flags", 12),
        ("reginfo0", 16),
        ("reginfo1", 20),
    ] {
        assert_eq!(g(&format!("offsetof.struct wmi_channel.{f}")), off, "{f}");
    }
    for (f, off) in [
        ("vdev_id", 0),
        ("desc_id", 4),
        ("chanfreq", 8),
        ("paddr", 12),
        ("frame_len", 20),
        ("buf_len", 24),
    ] {
        assert_eq!(
            g(&format!("offsetof.struct wmi_tlv_mgmt_tx_cmd.{f}")),
            off,
            "{f}"
        );
    }

    let consts: &[(&str, u64)] = &[
        (
            "WMI_TLV_MGMT_TX_FRAME_MAX_LEN",
            MGMT_TX_FRAME_MAX_LEN as u64,
        ),
        (
            "WMI_RSRC_CFG_FLAG_TX_ACK_RSSI",
            u64::from(RSRC_CFG_FLAG_TX_ACK_RSSI),
        ),
        (
            "WMI_TLV_FLAG_MGMT_BUNDLE_TX_COMPL",
            u64::from(FLAG_MGMT_BUNDLE_TX_COMPL),
        ),
        ("WMI_TLV_ABI_VER0", u64::from(ABI_VER0)),
        ("WMI_TLV_ABI_VER1", u64::from(ABI_VER1)),
        ("WMI_TLV_ABI_VER_NS0", u64::from(ABI_VER_NS0)),
        ("WMI_TLV_ABI_VER_NS1", u64::from(ABI_VER_NS1)),
        ("WLAN_SCAN_PARAMS_MAX_SSID", SCAN_MAX_SSID as u64),
        ("WLAN_SCAN_PARAMS_MAX_BSSID", SCAN_MAX_BSSID as u64),
        ("WLAN_SCAN_PARAMS_MAX_IE_LEN", SCAN_MAX_IE_LEN as u64),
        ("WMI_HOST_SCAN_REQ_ID_PREFIX", u64::from(SCAN_ID_PREFIX)),
        (
            "WMI_HOST_SCAN_REQUESTOR_ID_PREFIX",
            u64::from(SCAN_ID_PREFIX),
        ),
        (
            "WMI_SCAN_FILTER_PROBE_REQ",
            u64::from(SCAN_FILTER_PROBE_REQ),
        ),
        ("WMI_SCAN_STOP_VDEV_ALL", 0x0100_0000),
        ("WMI_SCAN_STOP_ALL", 0x0400_0000),
        (
            "WMI_VDEV_START_HIDDEN_SSID",
            u64::from(VDEV_START_HIDDEN_SSID),
        ),
        (
            "WMI_VDEV_START_PMF_ENABLED",
            u64::from(VDEV_START_PMF_ENABLED),
        ),
        ("WMI_CHAN_FLAG_HT40_PLUS", u64::from(chan_flag::HT40_PLUS)),
        ("WMI_CHAN_FLAG_PASSIVE", u64::from(chan_flag::PASSIVE)),
        (
            "WMI_CHAN_FLAG_ADHOC_ALLOWED",
            u64::from(chan_flag::ADHOC_ALLOWED),
        ),
        ("WMI_CHAN_FLAG_DFS", u64::from(chan_flag::DFS)),
        ("WMI_CHAN_FLAG_ALLOW_HT", u64::from(chan_flag::ALLOW_HT)),
        ("WMI_CHAN_FLAG_ALLOW_VHT", u64::from(chan_flag::ALLOW_VHT)),
        ("WMI_CHAN_FLAG_DFS_CFREQ2", u64::from(chan_flag::DFS_CFREQ2)),
        ("WMI_VDEV_TYPE_AP", u64::from(vdev_type::AP)),
        ("WMI_VDEV_TYPE_STA", u64::from(vdev_type::STA)),
        ("WMI_VDEV_TYPE_IBSS", u64::from(vdev_type::IBSS)),
        ("WMI_VDEV_TYPE_MONITOR", u64::from(vdev_type::MONITOR)),
        ("WMI_TLV_VDEV_SUBTYPE_NONE", u64::from(vdev_subtype::NONE)),
        ("WMI_PEER_TYPE_DEFAULT", u64::from(peer_type::DEFAULT)),
        ("WMI_PEER_TYPE_BSS", u64::from(peer_type::BSS)),
        ("WMI_PEER_TYPE_TDLS", u64::from(peer_type::TDLS)),
        ("MODE_11A", u64::from(phy_mode::MODE_11A)),
        ("MODE_11G", u64::from(phy_mode::MODE_11G)),
        ("MODE_11B", u64::from(phy_mode::MODE_11B)),
        ("MODE_11GONLY", u64::from(phy_mode::MODE_11GONLY)),
        ("MODE_11NA_HT20", u64::from(phy_mode::MODE_11NA_HT20)),
        ("MODE_11NG_HT20", u64::from(phy_mode::MODE_11NG_HT20)),
        ("MODE_11NA_HT40", u64::from(phy_mode::MODE_11NA_HT40)),
        ("MODE_11NG_HT40", u64::from(phy_mode::MODE_11NG_HT40)),
        ("MODE_11AC_VHT20", u64::from(phy_mode::MODE_11AC_VHT20)),
        ("MODE_11AC_VHT40", u64::from(phy_mode::MODE_11AC_VHT40)),
        ("MODE_11AC_VHT80", u64::from(phy_mode::MODE_11AC_VHT80)),
        (
            "MODE_11AC_VHT20_2G",
            u64::from(phy_mode::MODE_11AC_VHT20_2G),
        ),
        (
            "MODE_11AC_VHT40_2G",
            u64::from(phy_mode::MODE_11AC_VHT40_2G),
        ),
        (
            "MODE_11AC_VHT80_2G",
            u64::from(phy_mode::MODE_11AC_VHT80_2G),
        ),
        (
            "MODE_11AC_VHT80_80",
            u64::from(phy_mode::MODE_11AC_VHT80_80),
        ),
        ("MODE_11AC_VHT160", u64::from(phy_mode::MODE_11AC_VHT160)),
        ("MODE_UNKNOWN", u64::from(phy_mode::MODE_UNKNOWN)),
        ("WMI_SCAN_EVENT_STARTED", u64::from(scan_event::STARTED)),
        ("WMI_SCAN_EVENT_COMPLETED", u64::from(scan_event::COMPLETED)),
        (
            "WMI_SCAN_EVENT_BSS_CHANNEL",
            u64::from(scan_event::BSS_CHANNEL),
        ),
        (
            "WMI_SCAN_EVENT_FOREIGN_CHANNEL",
            u64::from(scan_event::FOREIGN_CHANNEL),
        ),
        ("WMI_SCAN_EVENT_DEQUEUED", u64::from(scan_event::DEQUEUED)),
        ("WMI_SCAN_EVENT_PREEMPTED", u64::from(scan_event::PREEMPTED)),
        (
            "WMI_SCAN_EVENT_START_FAILED",
            u64::from(scan_event::START_FAILED),
        ),
        ("WMI_SCAN_EVENT_RESTARTED", u64::from(scan_event::RESTARTED)),
        (
            "WMI_SCAN_EVENT_FOREIGN_CHANNEL_EXIT",
            u64::from(scan_event::FOREIGN_CHANNEL_EXIT),
        ),
        ("WMI_MAX_MEM_REQS", MAX_MEM_REQS as u64),
        ("TARGET_HL_TLV_NUM_PEERS", u64::from(wcn3990::NUM_PEERS)),
        (
            "TARGET_HL_TLV_AST_SKID_LIMIT",
            u64::from(wcn3990::AST_SKID_LIMIT),
        ),
        (
            "TARGET_HL_TLV_NUM_WDS_ENTRIES",
            u64::from(wcn3990::NUM_WDS_ENTRIES),
        ),
        ("TARGET_TLV_NUM_VDEVS", u64::from(wcn3990::NUM_VDEVS)),
        (
            "TARGET_TLV_NUM_TDLS_VDEVS",
            u64::from(wcn3990::NUM_TDLS_VDEVS),
        ),
    ];
    for (n, want) in consts {
        assert_eq!(g(&format!("const.{n}")), *want, "{n}");
    }
    assert_eq!(g("const.TARGET_TLV_NUM_PEERS"), 33);
    assert_eq!(g("const.TARGET_TLV_NUM_TIDS"), 66);

    let ids: &[(&str, u32)] = &[
        ("WMI_TLV_INIT_CMDID", cmd::INIT),
        ("WMI_TLV_START_SCAN_CMDID", cmd::START_SCAN),
        ("WMI_TLV_STOP_SCAN_CMDID", cmd::STOP_SCAN),
        ("WMI_TLV_PDEV_SET_REGDOMAIN_CMDID", cmd::PDEV_SET_REGDOMAIN),
        ("WMI_TLV_PDEV_SET_PARAM_CMDID", cmd::PDEV_SET_PARAM),
        ("WMI_TLV_VDEV_CREATE_CMDID", cmd::VDEV_CREATE),
        ("WMI_TLV_VDEV_DELETE_CMDID", cmd::VDEV_DELETE),
        ("WMI_TLV_VDEV_START_REQUEST_CMDID", cmd::VDEV_START_REQUEST),
        ("WMI_TLV_VDEV_UP_CMDID", cmd::VDEV_UP),
        ("WMI_TLV_VDEV_STOP_CMDID", cmd::VDEV_STOP),
        ("WMI_TLV_VDEV_DOWN_CMDID", cmd::VDEV_DOWN),
        ("WMI_TLV_VDEV_INSTALL_KEY_CMDID", cmd::VDEV_INSTALL_KEY),
        ("WMI_TLV_PEER_CREATE_CMDID", cmd::PEER_CREATE),
        ("WMI_TLV_PEER_DELETE_CMDID", cmd::PEER_DELETE),
        ("WMI_TLV_PEER_ASSOC_CMDID", cmd::PEER_ASSOC),
        ("WMI_TLV_MGMT_TX_CMDID", cmd::MGMT_TX),
        ("WMI_TLV_SERVICE_READY_EVENTID", event::SERVICE_READY),
        ("WMI_TLV_READY_EVENTID", event::READY),
        (
            "WMI_TLV_SERVICE_AVAILABLE_EVENTID",
            event::SERVICE_AVAILABLE,
        ),
        ("WMI_TLV_SCAN_EVENTID", event::SCAN),
        ("WMI_TLV_CHAN_INFO_EVENTID", event::CHAN_INFO),
        ("WMI_TLV_TX_PAUSE_EVENTID", event::TX_PAUSE),
        ("WMI_TLV_VDEV_START_RESP_EVENTID", event::VDEV_START_RESP),
        ("WMI_TLV_VDEV_STOPPED_EVENTID", event::VDEV_STOPPED),
        ("WMI_TLV_VDEV_DELETE_RESP_EVENTID", event::VDEV_DELETE_RESP),
        ("WMI_TLV_PEER_STA_KICKOUT_EVENTID", event::PEER_STA_KICKOUT),
        ("WMI_TLV_PEER_DELETE_RESP_EVENTID", event::PEER_DELETE_RESP),
        ("WMI_TLV_MGMT_RX_EVENTID", event::MGMT_RX),
        (
            "WMI_TLV_MGMT_TX_COMPLETION_EVENTID",
            event::MGMT_TX_COMPLETION,
        ),
    ];
    for (n, want) in ids {
        assert_eq!(g(&format!("id.{n}")) as u32, *want, "{n}");
    }
    let tags: &[(&str, u16)] = &[
        ("ARRAY_UINT32", tag::ARRAY_UINT32),
        ("ARRAY_BYTE", tag::ARRAY_BYTE),
        ("ARRAY_STRUCT", tag::ARRAY_STRUCT),
        ("ARRAY_FIXED_STRUCT", tag::ARRAY_FIXED_STRUCT),
        ("STRUCT_INIT_CMD", tag::INIT_CMD),
        ("STRUCT_RESOURCE_CONFIG", tag::RESOURCE_CONFIG),
        ("STRUCT_WLAN_HOST_MEMORY_CHUNK", tag::WLAN_HOST_MEMORY_CHUNK),
        ("STRUCT_START_SCAN_CMD", tag::START_SCAN_CMD),
        ("STRUCT_STOP_SCAN_CMD", tag::STOP_SCAN_CMD),
        ("STRUCT_VDEV_CREATE_CMD", tag::VDEV_CREATE_CMD),
        ("STRUCT_VDEV_DELETE_CMD", tag::VDEV_DELETE_CMD),
        ("STRUCT_VDEV_START_REQUEST_CMD", tag::VDEV_START_REQUEST_CMD),
        ("STRUCT_VDEV_UP_CMD", tag::VDEV_UP_CMD),
        ("STRUCT_VDEV_DOWN_CMD", tag::VDEV_DOWN_CMD),
        ("STRUCT_PEER_CREATE_CMD", tag::PEER_CREATE_CMD),
        ("STRUCT_PEER_DELETE_CMD", tag::PEER_DELETE_CMD),
        ("STRUCT_PDEV_SET_REGDOMAIN_CMD", tag::PDEV_SET_REGDOMAIN_CMD),
        ("STRUCT_CHANNEL", tag::CHANNEL),
        ("STRUCT_MGMT_TX_CMD", tag::MGMT_TX_CMD),
        ("STRUCT_SERVICE_READY_EVENT", tag::SERVICE_READY_EVENT),
        ("STRUCT_HAL_REG_CAPABILITIES", tag::HAL_REG_CAPABILITIES),
        ("STRUCT_WLAN_HOST_MEM_REQ", tag::WLAN_HOST_MEM_REQ),
        ("STRUCT_READY_EVENT", tag::READY_EVENT),
        (
            "STRUCT_SERVICE_AVAILABLE_EVENT",
            tag::SERVICE_AVAILABLE_EVENT,
        ),
        ("STRUCT_SCAN_EVENT", tag::SCAN_EVENT),
        ("STRUCT_MGMT_RX_HDR", tag::MGMT_RX_HDR),
        ("STRUCT_CHAN_INFO_EVENT", tag::CHAN_INFO_EVENT),
        (
            "STRUCT_VDEV_START_RESPONSE_EVENT",
            tag::VDEV_START_RESPONSE_EVENT,
        ),
        ("STRUCT_PEER_STA_KICKOUT_EVENT", tag::PEER_STA_KICKOUT_EVENT),
        ("STRUCT_HOST_SWBA_EVENT", tag::HOST_SWBA_EVENT),
        ("STRUCT_TIM_INFO", tag::TIM_INFO),
        ("STRUCT_P2P_NOA_INFO", tag::P2P_NOA_INFO),
        (
            "STRUCT_OFFLOAD_BCN_TX_STATUS_EVENT",
            tag::OFFLOAD_BCN_TX_STATUS_EVENT,
        ),
        (
            "STRUCT_DIAG_DATA_CONTAINER_EVENT",
            tag::DIAG_DATA_CONTAINER_EVENT,
        ),
        ("STRUCT_P2P_NOA_EVENT", tag::P2P_NOA_EVENT),
        ("STRUCT_ROAM_EVENT", tag::ROAM_EVENT),
        ("STRUCT_WOW_EVENT_INFO", tag::WOW_EVENT_INFO),
        ("STRUCT_TX_PAUSE_EVENT", tag::TX_PAUSE_EVENT),
    ];
    for (n, want) in tags {
        assert_eq!(g(&format!("tag.WMI_TLV_TAG_{n}")) as u16, *want, "{n}");
    }
    // the policy table, both ways
    let mut c_policy: Vec<(u16, usize)> = FIX
        .lines()
        .filter_map(|l| l.strip_prefix("policy."))
        .map(|l| {
            let (t, m) = l.split_once(' ').unwrap();
            (t.parse().unwrap(), m.parse().unwrap())
        })
        .collect();
    let mut rust_policy = POLICY.to_vec();
    c_policy.sort();
    rust_policy.sort();
    assert_eq!(rust_policy, c_policy);
}

#[test]
fn iteration_matches_ath10k_wmi_tlv_iter() {
    let inputs: Vec<Vec<u8>> = vec![
        vec![4, 0, 1, 0, 1, 2, 3, 4, 0, 0, 2, 0],
        vec![4, 0, 1, 0, 1, 2, 3, 4, 7, 7],
        vec![9, 0, 1, 0, 1, 2, 3, 4],
        vec![3, 0, 1, 0, 1, 2, 3, 0, 0, 0, 2, 0],
        {
            let mut v = vec![8, 0];
            v.extend_from_slice(&tag::SCAN_EVENT.to_le_bytes());
            v.extend_from_slice(&[0; 8]);
            v
        },
        {
            let mut v = vec![24, 0];
            v.extend_from_slice(&tag::SCAN_EVENT.to_le_bytes());
            v.extend_from_slice(&[0; 24]);
            v
        },
        {
            let mut v = vec![0, 0];
            v.extend_from_slice(&tag::ARRAY_UINT32.to_le_bytes());
            v
        },
        vec![],
    ];
    for (i, input) in inputs.iter().enumerate() {
        let line = line_after(&format!("iter.{} ", i + 1));
        let (body, ret) = line.rsplit_once(" ret=").unwrap();
        let ret: i32 = ret.parse().unwrap();
        let mut seen = String::new();
        let mut r = 0;
        for t in iter(input) {
            match t {
                Ok(t) => seen.push_str(&format!(
                    " tlv(tag={},len={},off={})",
                    t.tag,
                    t.value.len(),
                    t.offset
                )),
                Err(e) => r = e.errno(),
            }
        }
        assert_eq!(format!("iter.{}{}", i + 1, seen), body, "case {}", i + 1);
        assert_eq!(r, ret, "case {}", i + 1);
    }
}

fn fixture_hex(name: &str) -> Vec<u8> {
    let l = line_after(&format!("{name} "));
    hex(l.split(' ').nth(2).unwrap())
}

fn field(line: &str, key: &str) -> u64 {
    let k = format!("{key}=");
    let s = line
        .split(' ')
        .find(|f| f.starts_with(&k))
        .unwrap_or_else(|| panic!("{key} in {line}"));
    num(&s[k.len()..])
}

#[test]
fn service_ready_parses_like_the_c() {
    for name in [
        "svc_rdy_ok",
        "svc_rdy_no_memreq",
        "svc_rdy_abi",
        "svc_rdy_no_reg",
        "svc_rdy_bad_memreq_tag",
        "svc_rdy_short_ev",
        "svc_rdy_many_memreqs",
    ] {
        let buf = fixture_hex(name);
        let idx = FIX
            .lines()
            .position(|l| l == format!("case {name}"))
            .unwrap();
        let lines: Vec<&str> = FIX.lines().skip(idx + 1).collect();
        let ret: i32 = lines[0]
            .strip_prefix("svc_rdy.ret ")
            .unwrap()
            .parse()
            .unwrap();
        match parse_service_ready(&buf) {
            Err(e) => assert_eq!(e.errno(), ret, "{name}"),
            Ok(s) => {
                assert_eq!(ret, 0, "{name}");
                let f = lines[1];
                assert_eq!(u64::from(s.min_tx_power), field(f, "min_tx_power"));
                assert_eq!(u64::from(s.max_tx_power), field(f, "max_tx_power"));
                assert_eq!(u64::from(s.ht_cap), field(f, "ht_cap"));
                assert_eq!(u64::from(s.vht_cap), field(f, "vht_cap"));
                assert_eq!(u64::from(s.vht_supp_mcs), field(f, "vht_supp_mcs"));
                assert_eq!(u64::from(s.sw_ver0), field(f, "sw_ver0"));
                assert_eq!(u64::from(s.sw_ver1), field(f, "sw_ver1"));
                assert_eq!(u64::from(s.fw_build), field(f, "fw_build"));
                assert_eq!(u64::from(s.phy_capab), field(f, "phy_capab"));
                assert_eq!(u64::from(s.num_rf_chains), field(f, "num_rf_chains"));
                assert_eq!(u64::from(s.eeprom_rd), field(f, "eeprom_rd"));
                assert_eq!(u64::from(s.num_mem_reqs), field(f, "num_mem_reqs"));
                assert_eq!(u64::from(s.low_2ghz_chan), field(f, "low2"));
                assert_eq!(u64::from(s.high_2ghz_chan), field(f, "high2"));
                assert_eq!(u64::from(s.low_5ghz_chan), field(f, "low5"));
                assert_eq!(u64::from(s.high_5ghz_chan), field(f, "high5"));
                assert_eq!(u64::from(s.sys_cap_info), field(f, "sys_cap_info"));
                assert_eq!(s.service_map.len() as u64, field(f, "service_map_len"));
                let off = s.service_map.as_ptr() as usize - buf.as_ptr() as usize;
                assert_eq!(off as u64, field(f, "service_map_off"));
                for (i, m) in s.mem_reqs.iter().enumerate() {
                    let l = lines[2 + i];
                    assert!(l.starts_with(&format!("svc_rdy.mem_req.{i} ")), "{l}");
                    assert_eq!(u64::from(m.req_id), field(l, "req_id"));
                    assert_eq!(u64::from(m.unit_size), field(l, "unit_size"));
                    assert_eq!(u64::from(m.num_unit_info), field(l, "num_unit_info"));
                    assert_eq!(u64::from(m.num_units), field(l, "num_units"));
                }
                assert!(
                    !lines[2 + s.mem_reqs.len()].starts_with("svc_rdy.mem_req"),
                    "{name}: C found more requests"
                );
                if name == "svc_rdy_ok" {
                    // the bitmap is 0x00ff00ff, 0x1, 0x80000000, 12 bytes: four bits per word below 12
                    assert!(s.service_enabled(0) && s.service_enabled(4) && !s.service_enabled(8));
                }
            }
        }
    }
}

#[test]
fn ready_scan_vdev_start_service_available_parse_like_the_c() {
    // ready
    let r = parse_ready(&fixture_hex("ready_ok")).unwrap();
    let f = line_after("ready.fields ");
    assert_eq!(u64::from(r.sw_version), field(f, "sw_version"));
    assert_eq!(u64::from(r.abi_version), field(f, "abi_version"));
    assert_eq!(u64::from(r.status), field(f, "status"));
    assert_eq!(to_hex(&r.mac_addr), f.rsplit_once("mac=").unwrap().1);
    assert_eq!(r.mac_addr, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let mut missing = Vec::new();
    missing.extend_from_slice(&4u16.to_le_bytes());
    missing.extend_from_slice(&tag::ARRAY_BYTE.to_le_bytes());
    missing.extend_from_slice(b"abcd");
    assert_eq!(
        parse_ready(&missing).unwrap_err().errno(),
        line_after("ready_missing.ret ")
            .split(' ')
            .nth(1)
            .unwrap()
            .parse::<i32>()
            .unwrap()
    );
    let ok = fixture_hex("ready_ok");
    assert_eq!(parse_ready(&ok[..ok.len() - 1]).unwrap_err().errno(), -22);
    // scan
    let s = parse_scan_event(&fixture_hex("scan_ev")).unwrap();
    let f = line_after("scan_ev.fields ");
    assert_eq!(u64::from(s.event_type), field(f, "event_type"));
    assert_eq!(u64::from(s.reason), field(f, "reason"));
    assert_eq!(u64::from(s.channel_freq), field(f, "channel_freq"));
    assert_eq!(u64::from(s.scan_req_id), field(f, "scan_req_id"));
    assert_eq!(u64::from(s.scan_id), field(f, "scan_id"));
    assert_eq!(u64::from(s.vdev_id), field(f, "vdev_id"));
    assert_eq!(parse_scan_event(&missing).unwrap_err(), WmiError::Protocol);
    // vdev start response
    let v = parse_vdev_start_response(&fixture_hex("vdev_start_resp")).unwrap();
    let f = line_after("vdev_start_resp.fields ");
    assert_eq!(
        (
            u64::from(v.vdev_id),
            u64::from(v.req_id),
            u64::from(v.resp_type),
            u64::from(v.status)
        ),
        (
            field(f, "vdev_id"),
            field(f, "req_id"),
            field(f, "resp_type"),
            field(f, "status")
        )
    );
    // service available
    let b = fixture_hex("svc_avail");
    let a = parse_service_available(&b).unwrap().unwrap();
    let f = line_after("svc_avail.ret ");
    assert_eq!(u64::from(a.service_map_ext_len), field(f, "len"));
    assert_eq!(
        (a.service_map_ext.as_ptr() as usize - b.as_ptr() as usize) as u64,
        field(f, "off")
    );
    assert_eq!(a.service_map_ext, &[0xa5u8; 8][..]);
    assert_eq!(parse_service_available(&missing), Ok(None));
    // a TLV shorter than the length word: the C would read out of bounds
    let mut short = Vec::new();
    short.extend_from_slice(&2u16.to_le_bytes());
    short.extend_from_slice(&tag::SERVICE_AVAILABLE_EVENT.to_le_bytes());
    short.extend_from_slice(&[1, 2]);
    assert_eq!(parse_service_available(&short), Err(WmiError::Invalid));
}

#[test]
fn event_header_and_dispatch() {
    let mut b = event::SCAN.to_le_bytes().to_vec();
    b.extend_from_slice(&[1, 2, 3]);
    assert_eq!(rx(&b), Ok((EventId::Scan, &[1u8, 2, 3][..])));
    // the top byte (plt_priv) is not part of the id
    let mut b = (0xab00_0000 | event::READY).to_le_bytes().to_vec();
    b.push(9);
    assert_eq!(rx(&b), Ok((EventId::Ready, &[9u8][..])));
    assert_eq!(rx(&[1, 2, 3]), Err(WmiError::Invalid));
    assert_eq!(
        rx(&0x1234u32.to_le_bytes()).unwrap().0,
        EventId::Other(0x1234)
    );
    assert_eq!(frame(cmd::INIT, &[7, 8]), vec![1, 0, 0, 0, 7, 8]);
}

#[test]
fn encoders_match_the_c_byte_for_byte() {
    let chunks = [
        MemChunk {
            req_id: 1,
            len: 0x20000,
            paddr: 0x1_2000_0000,
        },
        MemChunk {
            req_id: 2,
            len: 0x4000,
            paddr: 0x1_3000_0000,
        },
        MemChunk {
            req_id: 0x13,
            len: 0x800,
            paddr: 0xfff0_0000,
        },
    ];
    let mut p = InitParams {
        chunks: &[],
        rx_decap_mode: 1,
        max_num_pending_tx: wcn3990::NUM_MSDU_DESC,
        max_num_wow_patterns: 0,
        svc_rx_full_reorder: false,
        svc_tx_data_ack_rssi: false,
        svc_support_extend_address: false,
    };
    check("cmd.init_no_chunks", gen_init(&p));
    p.chunks = &chunks;
    check("cmd.init_chunks_32", gen_init(&p));
    p.svc_rx_full_reorder = true;
    p.svc_tx_data_ack_rssi = true;
    p.svc_support_extend_address = true;
    p.max_num_wow_patterns = 22;
    p.rx_decap_mode = 2;
    check("cmd.init_all_services", gen_init(&p));

    let ssid_b = b"0123456789abcdef0123456789abcdef";
    let ie = [0xddu8, 8, 0, 0x50, 0xf2, 4, 1, 2, 3, 4];
    let mut a = StartScanArg {
        scan_id: 0x123,
        scan_req_id: 0x45,
        vdev_id: 1,
        scan_priority: 2,
        notify_scan_events: 0x1ff,
        dwell_time_active: 50,
        dwell_time_passive: 110,
        min_rest_time: 50,
        max_rest_time: 500,
        idle_time: 25,
        max_scan_time: 20000,
        probe_delay: 5,
        scan_ctrl_flags: 0x2 | 0x4,
        ie: &ie,
        channels: &[2412, 2437, 5180],
        ssids: &[b"abc", ssid_b],
        bssids: &[[2, 3, 4, 5, 6, 7]],
        mac_addr: [0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f],
        mac_mask: [0xff; 6],
        ..StartScanArg::default()
    };
    check("cmd.start_scan_full", gen_start_scan(&a));
    a.scan_ctrl_flags = SCAN_FILTER_PROBE_REQ | 0x1;
    a.ie = &[];
    a.channels = &[];
    a.ssids = &[];
    a.bssids = &[];
    check("cmd.start_scan_empty", gen_start_scan(&a));
    a.ie = &ie[..5];
    a.channels = &[2412];
    a.ssids = &[b""];
    check("cmd.start_scan_odd_ie", gen_start_scan(&a));
    let big_ie = [0u8; 257];
    a.ie = &big_ie;
    check("cmd.start_scan_ie_too_long", gen_start_scan(&a));
    a.ie = &[];
    let chans = [0u16; 65];
    a.channels = &chans;
    check("cmd.start_scan_too_many_channels", gen_start_scan(&a));
    a.channels = &[];
    let ssids = [&b""[..]; 17];
    a.ssids = &ssids;
    check("cmd.start_scan_too_many_ssids", gen_start_scan(&a));
    a.ssids = &[];
    let bssids = [[0u8; 6]; 5];
    a.bssids = &bssids;
    check("cmd.start_scan_too_many_bssids", gen_start_scan(&a));
    // an SSID the C would copy 33 bytes of
    a.bssids = &[];
    let long = [0u8; 33];
    let long_list = [&long[..]];
    a.ssids = &long_list;
    assert_eq!(gen_start_scan(&a), Err(WmiError::Invalid));

    check(
        "cmd.stop_scan_one",
        gen_stop_scan(7, StopScan::One { scan_id: 0x123 }),
    );
    check(
        "cmd.stop_scan_vdev",
        gen_stop_scan(7, StopScan::VdevAll { vdev_id: 3 }),
    );
    check("cmd.stop_scan_all", gen_stop_scan(7, StopScan::All));
    check(
        "cmd.stop_scan_bad_req",
        gen_stop_scan(0x1000, StopScan::All),
    );
    check(
        "cmd.stop_scan_bad_id",
        gen_stop_scan(1, StopScan::One { scan_id: 0x1000 }),
    );

    let mac = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
    let bss = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01];
    check(
        "cmd.vdev_create",
        Ok(gen_vdev_create(1, vdev_type::STA, vdev_subtype::NONE, &mac)),
    );
    check("cmd.vdev_delete", Ok(gen_vdev_delete(1)));
    check("cmd.vdev_up", Ok(gen_vdev_up(1, 0x4001, &bss)));
    check("cmd.vdev_down", Ok(gen_vdev_down(1)));
    check(
        "cmd.peer_create",
        Ok(gen_peer_create(1, &bss, peer_type::BSS)),
    );
    check("cmd.peer_delete", Ok(gen_peer_delete(1, &bss)));
    check(
        "cmd.pdev_set_rd",
        Ok(gen_pdev_set_regdomain(0x1111, 0x2222, 0x3333, 0x44, 0x55)),
    );

    let mut v = VdevStartArg {
        vdev_id: 1,
        bcn_intval: 100,
        dtim_period: 2,
        ssid: Some(b"lucina"),
        pmf_enabled: true,
        channel: ChannelArg {
            freq: 5180,
            band_center_freq1: 5190,
            allow_ht: true,
            allow_vht: true,
            ht40plus: true,
            chan_radar: true,
            max_power: 60,
            max_reg_power: 54,
            max_antenna_gain: 3,
            reg_class_id: 0x80,
            mode: phy_mode::MODE_11AC_VHT40,
            ..ChannelArg::default()
        },
        ..VdevStartArg::default()
    };
    check("cmd.vdev_start", gen_vdev_start(&v));
    v.ssid = None;
    v.pmf_enabled = false;
    v.disable_hw_ack = true;
    v.channel = ChannelArg {
        freq: 2437,
        band_center_freq1: 2437,
        passive: true,
        max_power: 60,
        max_reg_power: 54,
        max_antenna_gain: 3,
        reg_class_id: 0x80,
        mode: phy_mode::MODE_11G,
        ..ChannelArg::default()
    };
    check("cmd.vdev_start_open", gen_vdev_start(&v));
    let long = [b'x'; 33];
    v.ssid = Some(&long);
    check("cmd.vdev_start_ssid_too_long", gen_vdev_start(&v));
    v.ssid = None;
    v.hidden_ssid = true;
    check("cmd.vdev_start_hidden_no_ssid", gen_vdev_start(&v));

    let frame_bytes: Vec<u8> = (0..100u8).collect();
    check(
        "cmd.mgmt_tx_100",
        Ok(gen_mgmt_tx_send(1, 7, 0x1_2345_6780, &frame_bytes)),
    );
    // a short frame is padded with zeros to a multiple of 4 and still reports its real length
    let m = gen_mgmt_tx_send(1, 7, 0, &frame_bytes[..30]);
    assert_eq!(m.len(), 4 + 28 + 4 + 32);
    assert_eq!(u32::from_le_bytes(m[24..28].try_into().unwrap()), 30);
    assert_eq!(u32::from_le_bytes(m[28..32].try_into().unwrap()), 32);
    assert_eq!(&m[36..], &[&frame_bytes[..30], &[0u8, 0][..]].concat()[..]);
}

#[test]
fn vht160_and_80_80_channel_words() {
    // not in the oracle (the C asks mac80211 for the second segment's radar flag); the arithmetic is from the C
    let mut o = Vec::new();
    put_channel(
        &mut o,
        &ChannelArg {
            freq: 5200,
            band_center_freq1: 5250,
            mode: phy_mode::MODE_11AC_VHT160,
            cfreq2_radar: true,
            ..ChannelArg::default()
        },
    );
    let wd = |i: usize| u32::from_le_bytes(o[4 * i..4 * i + 4].try_into().unwrap());
    assert_eq!((wd(0), wd(1), wd(2)), (5200, 5210, 5250)); // freq < centre: centre1 - 40, centre2 = old centre1
    assert_eq!(
        wd(3),
        u32::from(phy_mode::MODE_11AC_VHT160) | chan_flag::DFS_CFREQ2
    );
    let mut o = Vec::new();
    put_channel(
        &mut o,
        &ChannelArg {
            freq: 5180,
            band_center_freq1: 5210,
            band_center_freq2: 5775,
            mode: phy_mode::MODE_11AC_VHT80_80,
            ..ChannelArg::default()
        },
    );
    assert_eq!(u32::from_le_bytes(o[8..12].try_into().unwrap()), 5775);
}

#[test]
fn resource_config_defaults_for_wcn3990() {
    let p = InitParams {
        chunks: &[],
        rx_decap_mode: 1,
        max_num_pending_tx: wcn3990::NUM_MSDU_DESC,
        max_num_wow_patterns: 0,
        svc_rx_full_reorder: false,
        svc_tx_data_ack_rssi: false,
        svc_support_extend_address: false,
    };
    let r = ResourceConfig::wcn3990(&p);
    assert_eq!(
        (
            r.num_vdevs,
            r.num_peers,
            r.num_tids,
            r.ast_skid_limit,
            r.num_wds_entries
        ),
        (4, 33, 66, 16, 2)
    );
    assert_eq!(r.host_capab, FLAG_MGMT_BUNDLE_TX_COMPL);
    assert_eq!(r.to_le_bytes().len(), 176);
}

fn cmd_ok(name: &str) -> Vec<u8> {
    cmd_line(name).unwrap()
}

#[test]
fn service_map_test_matches_svcmap_of_the_header() {
    let map = cmd_ok("svcmap.map");
    for (name, len, from) in [
        ("svcmap.main.16", 16u32, 0u32),
        ("svcmap.main.48", 48, 0),
        ("svcmap.ext", svc::MAX_SERVICE, svc::MAX_SERVICE),
    ] {
        let want: Vec<u32> = line_after(&format!("{name} "))
            .split(' ')
            .skip(1)
            .map(|x| x.parse().unwrap())
            .collect();
        let got: Vec<u32> = (from..256)
            .filter(|x| svc_is_enabled(&map, *x, len))
            .collect();
        assert_eq!(got, want, "{name}");
    }
    assert!(!svc_is_enabled(&map, 256, 48));
    assert!(!svc_is_enabled(&[], 0, 0));
    assert!(!svc_is_enabled(&map, 100_000, 200_000));
}

#[test]
fn station_ids_tags_params_match_the_headers() {
    let k = kvl(FIX);
    let g = |n: &str| num(&k[n]);
    for (n, v) in [
        ("id.WMI_TLV_SCAN_CHAN_LIST_CMDID", cmd::SCAN_CHAN_LIST),
        ("id.WMI_TLV_SCAN_PROB_REQ_OUI_CMDID", cmd::SCAN_PROB_REQ_OUI),
        ("id.WMI_TLV_VDEV_SET_PARAM_CMDID", cmd::VDEV_SET_PARAM),
        ("id.WMI_TLV_PEER_SET_PARAM_CMDID", cmd::PEER_SET_PARAM),
        (
            "id.WMI_TLV_STA_POWERSAVE_MODE_CMDID",
            cmd::STA_POWERSAVE_MODE,
        ),
        (
            "id.WMI_TLV_STA_POWERSAVE_PARAM_CMDID",
            cmd::STA_POWERSAVE_PARAM,
        ),
        ("id.WMI_TLV_STA_KEEPALIVE_CMDID", cmd::STA_KEEPALIVE),
        (
            "id.WMI_TLV_RESMGR_ADAPTIVE_OCS_CMDID",
            cmd::RESMGR_ADAPTIVE_OCS,
        ),
        ("id.WMI_TLV_MGMT_TX_SEND_CMD", cmd::MGMT_TX_SEND),
        ("id.WMI_TLV_ROAM_EVENTID", event::ROAM),
        (
            "id.WMI_TLV_VDEV_INSTALL_KEY_COMPLETE_EVENTID",
            event::VDEV_INSTALL_KEY_COMPLETE,
        ),
        (
            "id.WMI_TLV_MGMT_TX_BUNDLE_COMPLETION_EVENTID",
            event::MGMT_TX_BUNDLE_COMPLETION,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_TX_CHAIN_MASK",
            pdev_param::TX_CHAIN_MASK,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_RX_CHAIN_MASK",
            pdev_param::RX_CHAIN_MASK,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_TXPOWER_LIMIT2G",
            pdev_param::TXPOWER_LIMIT2G,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_TXPOWER_LIMIT5G",
            pdev_param::TXPOWER_LIMIT5G,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_DYNAMIC_BW",
            pdev_param::DYNAMIC_BW,
        ),
        ("const.WMI_TLV_PDEV_PARAM_PMF_QOS", pdev_param::PMF_QOS),
        (
            "const.WMI_TLV_PDEV_PARAM_ARP_AC_OVERRIDE",
            pdev_param::ARP_AC_OVERRIDE,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_ANI_ENABLE",
            pdev_param::ANI_ENABLE,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_IDLE_PS_CONFIG",
            pdev_param::IDLE_PS_CONFIG,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_BURST_ENABLE",
            pdev_param::BURST_ENABLE,
        ),
        (
            "const.WMI_TLV_PDEV_PARAM_PEER_STATS_INFO_ENABLE",
            pdev_param::PEER_STATS_INFO_ENABLE,
        ),
        (
            "const.WMI_TLV_VDEV_PARAM_RTS_THRESHOLD",
            vdev_param::RTS_THRESHOLD,
        ),
        (
            "const.WMI_TLV_VDEV_PARAM_BEACON_INTERVAL",
            vdev_param::BEACON_INTERVAL,
        ),
        (
            "const.WMI_TLV_VDEV_PARAM_DTIM_PERIOD",
            vdev_param::DTIM_PERIOD,
        ),
        ("const.WMI_TLV_VDEV_PARAM_SLOT_TIME", vdev_param::SLOT_TIME),
        ("const.WMI_TLV_VDEV_PARAM_PREAMBLE", vdev_param::PREAMBLE),
        ("const.WMI_TLV_VDEV_PARAM_NSS", vdev_param::NSS),
        (
            "const.WMI_TLV_VDEV_PARAM_TX_ENCAP_TYPE",
            vdev_param::TX_ENCAP_TYPE,
        ),
        ("const.WMI_TLV_VDEV_PARAM_TXBF", vdev_param::TXBF),
        (
            "const.WMI_VDEV_PARAM_TXBF_SU_TX_BFEE",
            vdev_param::TXBF_SU_TX_BFEE,
        ),
        (
            "const.WMI_VDEV_PARAM_TXBF_MU_TX_BFEE",
            vdev_param::TXBF_MU_TX_BFEE,
        ),
        (
            "const.WMI_VDEV_PARAM_TXBF_SU_TX_BFER",
            vdev_param::TXBF_SU_TX_BFER,
        ),
        (
            "const.WMI_VDEV_PARAM_TXBF_MU_TX_BFER",
            vdev_param::TXBF_MU_TX_BFER,
        ),
        ("const.WMI_TLV_PEER_SMPS_STATE", peer_param::SMPS_STATE),
        ("const.WMI_TLV_PEER_AUTHORIZE", peer_param::AUTHORIZE),
        ("const.WMI_TLV_PEER_DUMMY_VAR", peer_param::DUMMY_VAR),
        ("const.WMI_TLV_PEER_AUTH", peer_flags::AUTH),
        ("const.WMI_TLV_PEER_QOS", peer_flags::QOS),
        (
            "const.WMI_TLV_PEER_NEED_PTK_4_WAY",
            peer_flags::NEED_PTK_4_WAY,
        ),
        (
            "const.WMI_TLV_PEER_NEED_GTK_2_WAY",
            peer_flags::NEED_GTK_2_WAY,
        ),
        ("const.WMI_TLV_PEER_HT", peer_flags::HT),
        ("const.WMI_TLV_PEER_40MHZ", peer_flags::BW40),
        ("const.WMI_TLV_PEER_STBC", peer_flags::STBC),
        ("const.WMI_TLV_PEER_LDPC", peer_flags::LDPC),
        ("const.WMI_TLV_PEER_VHT", peer_flags::VHT),
        ("const.WMI_TLV_PEER_80MHZ", peer_flags::BW80),
        ("const.WMI_TLV_PEER_160MHZ", peer_flags::BW160),
        ("const.WMI_TLV_PEER_PMF", peer_flags::PMF),
        ("const.WMI_RC_DS_FLAG", rate_caps::DS),
        ("const.WMI_RC_CW40_FLAG", rate_caps::CW40),
        ("const.WMI_RC_SGI_FLAG", rate_caps::SGI),
        ("const.WMI_RC_HT_FLAG", rate_caps::HT),
        ("const.WMI_RC_TX_STBC_FLAG", rate_caps::TX_STBC),
        ("const.WMI_RC_RX_STBC_FLAG_S", rate_caps::RX_STBC_SHIFT),
        ("const.WMI_RC_TS_FLAG", rate_caps::TS),
        ("const.WMI_KEY_PAIRWISE", key_flags::PAIRWISE),
        ("const.WMI_KEY_GROUP", key_flags::GROUP),
        ("const.WMI_TLV_CIPHER_NONE", cipher::NONE),
        ("const.WMI_TLV_CIPHER_TKIP", cipher::TKIP),
        ("const.WMI_TLV_CIPHER_AES_CCM", cipher::AES_CCM),
        ("const.WMI_MAX_KEY_INDEX", MAX_KEY_INDEX),
        (
            "const.WMI_STA_PS_PARAM_RX_WAKE_POLICY",
            sta_ps::PARAM_RX_WAKE_POLICY,
        ),
        (
            "const.WMI_STA_PS_PARAM_TX_WAKE_THRESHOLD",
            sta_ps::PARAM_TX_WAKE_THRESHOLD,
        ),
        (
            "const.WMI_STA_PS_PARAM_PSPOLL_COUNT",
            sta_ps::PARAM_PSPOLL_COUNT,
        ),
        (
            "const.WMI_STA_PS_RX_WAKE_POLICY_WAKE",
            sta_ps::RX_WAKE_POLICY_WAKE,
        ),
        (
            "const.WMI_STA_PS_TX_WAKE_THRESHOLD_ALWAYS",
            sta_ps::TX_WAKE_THRESHOLD_ALWAYS,
        ),
        (
            "const.WMI_STA_PS_PSPOLL_COUNT_NO_MAX",
            sta_ps::PSPOLL_COUNT_NO_MAX,
        ),
        ("const.WMI_STA_PS_MODE_DISABLED", sta_ps::MODE_DISABLED),
        ("const.WMI_STA_PS_MODE_ENABLED", sta_ps::MODE_ENABLED),
        ("const.WMI_RX_STATUS_ERR_CRC", rx_status::ERR_CRC),
        ("const.WMI_RX_STATUS_ERR_DECRYPT", rx_status::ERR_DECRYPT),
        ("const.WMI_RX_STATUS_ERR_MIC", rx_status::ERR_MIC),
        (
            "const.WMI_RX_STATUS_ERR_KEY_CACHE_MISS",
            rx_status::ERR_KEY_CACHE_MISS,
        ),
        ("const.WMI_RX_STATUS_EXT_INFO", rx_status::EXT_INFO),
        ("const.WMI_ROAM_REASON_BEACON_MISS", ROAM_REASON_BEACON_MISS),
        ("const.WMI_VDEV_SLOT_TIME_LONG", VDEV_SLOT_TIME_LONG),
        ("const.WMI_VDEV_SLOT_TIME_SHORT", VDEV_SLOT_TIME_SHORT),
        ("const.WMI_VDEV_PREAMBLE_LONG", VDEV_PREAMBLE_LONG),
        ("const.WMI_VDEV_PREAMBLE_SHORT", VDEV_PREAMBLE_SHORT),
        ("svc.BEACON_OFFLOAD", svc::BEACON_OFFLOAD),
        ("svc.STA_KEEP_ALIVE", svc::STA_KEEP_ALIVE),
        ("svc.BURST", svc::BURST),
        ("svc.ADAPTIVE_OCS", svc::ADAPTIVE_OCS),
        ("svc.RX_FULL_REORDER", svc::RX_FULL_REORDER),
        ("svc.MGMT_TX_WMI", svc::MGMT_TX_WMI),
        ("svc.SPOOF_MAC_SUPPORT", svc::SPOOF_MAC_SUPPORT),
        ("svc.TX_DATA_MGMT_ACK_RSSI", svc::TX_DATA_MGMT_ACK_RSSI),
        ("svc.SUPPORT_EXTEND_ADDRESS", svc::SUPPORT_EXTEND_ADDRESS),
        ("const.WMI_TLV_MAX_SERVICE", svc::MAX_SERVICE),
    ] {
        assert_eq!(g(n), u64::from(v), "{n}");
    }
    for (n, v) in [
        (
            "tag.WMI_TLV_TAG_STRUCT_PDEV_SET_PARAM_CMD",
            tag::PDEV_SET_PARAM_CMD,
        ),
        ("tag.WMI_TLV_TAG_STRUCT_VDEV_STOP_CMD", tag::VDEV_STOP_CMD),
        (
            "tag.WMI_TLV_TAG_STRUCT_VDEV_SET_PARAM_CMD",
            tag::VDEV_SET_PARAM_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_VDEV_INSTALL_KEY_CMD",
            tag::VDEV_INSTALL_KEY_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_PEER_SET_PARAM_CMD",
            tag::PEER_SET_PARAM_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_PEER_ASSOC_COMPLETE_CMD",
            tag::PEER_ASSOC_COMPLETE_CMD,
        ),
        ("tag.WMI_TLV_TAG_STRUCT_VHT_RATE_SET", tag::VHT_RATE_SET),
        (
            "tag.WMI_TLV_TAG_STRUCT_STA_POWERSAVE_MODE_CMD",
            tag::STA_POWERSAVE_MODE_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_STA_POWERSAVE_PARAM_CMD",
            tag::STA_POWERSAVE_PARAM_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_STA_KEEPALIVE_CMD",
            tag::STA_KEEPALIVE_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_STA_KEEPALVE_ARP_RESPONSE",
            tag::STA_KEEPALVE_ARP_RESPONSE,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_SCAN_CHAN_LIST_CMD",
            tag::SCAN_CHAN_LIST_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_SCAN_PROB_REQ_OUI_CMD",
            tag::SCAN_PROB_REQ_OUI_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_RESMGR_ADAPTIVE_OCS_CMD",
            tag::RESMGR_ADAPTIVE_OCS_CMD,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_MGMT_TX_COMPL_EVENT",
            tag::MGMT_TX_COMPL_EVENT,
        ),
        (
            "tag.WMI_TLV_TAG_STRUCT_MGMT_TX_COMPL_BUNDLE_EVENT",
            tag::MGMT_TX_COMPL_BUNDLE_EVENT,
        ),
    ] {
        assert_eq!(g(n), u64::from(v), "{n}");
    }
    assert_eq!(
        g("sizeof.struct wmi_vdev_install_key_cmd") as usize,
        INSTALL_KEY_CMD_LEN
    );
    assert_eq!(
        g("sizeof.struct wmi_tlv_peer_assoc_cmd") as usize,
        PEER_ASSOC_CMD_LEN
    );
    assert_eq!(g("const.MAX_SUPPORTED_RATES") as usize, MAX_SUPPORTED_RATES);
}

#[test]
fn station_commands_match_the_linux_encoders() {
    const BSS: [u8; 6] = [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01];
    let ok = |n: &str, v: Vec<u8>| check(n, Ok(v));
    ok(
        "cmd.pdev_set_param",
        gen_pdev_set_param(pdev_param::PMF_QOS, 1),
    );
    ok("cmd.vdev_stop", gen_vdev_stop(1));
    ok(
        "cmd.vdev_set_param",
        gen_vdev_set_param(1, vdev_param::TX_ENCAP_TYPE, 1),
    );
    ok(
        "cmd.peer_set_param",
        gen_peer_set_param(1, &BSS, peer_param::AUTHORIZE, 1),
    );
    ok("cmd.set_psmode", gen_set_psmode(1, sta_ps::MODE_DISABLED));
    ok(
        "cmd.set_sta_ps",
        gen_set_sta_ps(
            1,
            sta_ps::PARAM_TX_WAKE_THRESHOLD,
            sta_ps::TX_WAKE_THRESHOLD_ALWAYS,
        ),
    );
    ok("cmd.scan_prob_req_oui", gen_scan_prob_req_oui(0x021122));
    ok("cmd.adaptive_qcs", gen_adaptive_qcs(true));
    ok("cmd.sta_keepalive", gen_sta_keepalive_disable(1));

    let ptk: [u8; 16] = core::array::from_fn(|i| i as u8 + 1);
    let tk: [u8; 32] = core::array::from_fn(|i| 0xa0 + i as u8);
    let mut k = InstallKeyArg {
        vdev_id: 1,
        macaddr: BSS,
        key_idx: 0,
        key_flags: key_flags::PAIRWISE,
        key_cipher: cipher::AES_CCM,
        key_len: 16,
        key_txmic_len: 0,
        key_rxmic_len: 0,
        data: Some(&ptk),
    };
    check("cmd.install_key_ptk", gen_vdev_install_key(&k));
    k.key_idx = 2;
    k.key_flags = key_flags::GROUP;
    check("cmd.install_key_gtk", gen_vdev_install_key(&k));
    k.key_idx = 1;
    k.key_len = 32;
    k.data = Some(&tk);
    k.key_txmic_len = 8;
    k.key_rxmic_len = 8;
    k.key_cipher = cipher::TKIP;
    check("cmd.install_key_tkip", gen_vdev_install_key(&k));
    k.key_idx = 0;
    k.key_len = 16;
    k.data = None;
    k.key_txmic_len = 0;
    k.key_rxmic_len = 0;
    k.key_flags = key_flags::PAIRWISE;
    k.key_cipher = cipher::NONE;
    check("cmd.install_key_delete", gen_vdev_install_key(&k));
    k.data = Some(&ptk);
    check("cmd.install_key_none_with_data", gen_vdev_install_key(&k));
    k.data = None;
    k.key_cipher = cipher::AES_CCM;
    check("cmd.install_key_ccm_no_data", gen_vdev_install_key(&k));
    k.data = Some(&ptk[..4]);
    assert_eq!(
        gen_vdev_install_key(&k),
        Err(WmiError::Invalid),
        "short key data"
    );
    k.key_len = 100;
    k.data = Some(&[0; 100]);
    assert_eq!(gen_vdev_install_key(&k), Err(WmiError::Invalid), "long key");

    let legacy: Vec<u8> = (0..5).map(|n| 12 + 12 * n).collect();
    let ht: Vec<u8> = (0..16).collect();
    let mut pa = PeerAssocArg {
        addr: BSS,
        vdev_id: 1,
        reassoc: false,
        aid: 5,
        flags: peer_flags::AUTH
            | peer_flags::QOS
            | peer_flags::NEED_PTK_4_WAY
            | peer_flags::HT
            | peer_flags::VHT,
        caps: 0x431,
        listen_intval: 10,
        ht_caps: 0x1ef,
        max_mpdu: 0xffff,
        mpdu_density: 8,
        rate_caps: rate_caps::HT | rate_caps::CW40,
        num_spatial_streams: 2,
        vht_caps: 0x339071b2,
        phymode: u32::from(phy_mode::MODE_11AC_VHT80),
        legacy_rates: &legacy,
        ht_rates: &ht,
        vht_rx_max_rate: 867,
        vht_rx_mcs_set: 0xfffa,
        vht_tx_max_rate: 867,
        vht_tx_mcs_set: 0xfffa,
    };
    check("cmd.peer_assoc", gen_peer_assoc(&pa));
    pa.reassoc = true;
    pa.legacy_rates = &[];
    pa.ht_rates = &[];
    check("cmd.peer_assoc_reassoc_empty", gen_peer_assoc(&pa));
    pa.mpdu_density = 17;
    check("cmd.peer_assoc_bad_density", gen_peer_assoc(&pa));
    pa.mpdu_density = 0;
    let many = [0u8; MAX_SUPPORTED_RATES + 1];
    pa.legacy_rates = &many;
    check("cmd.peer_assoc_too_many_rates", gen_peer_assoc(&pa));

    let chans = [
        ChannelArg {
            freq: 2412,
            band_center_freq1: 2412,
            allow_ht: true,
            allow_vht: true,
            allow_ibss: true,
            ht40plus: true,
            max_power: 40,
            max_reg_power: 40,
            mode: phy_mode::MODE_11G,
            ..ChannelArg::default()
        },
        ChannelArg {
            freq: 2467,
            band_center_freq1: 2467,
            allow_ht: true,
            allow_vht: true,
            passive: true,
            max_power: 40,
            max_reg_power: 40,
            mode: phy_mode::MODE_11G,
            ..ChannelArg::default()
        },
        ChannelArg {
            freq: 5260,
            band_center_freq1: 5260,
            allow_ht: true,
            allow_vht: true,
            passive: true,
            chan_radar: true,
            ht40plus: true,
            max_power: 60,
            max_reg_power: 60,
            max_antenna_gain: 3,
            mode: phy_mode::MODE_11A,
            ..ChannelArg::default()
        },
    ];
    check("cmd.scan_chan_list", gen_scan_chan_list(&chans));
    check("cmd.scan_chan_list_empty", gen_scan_chan_list(&[]));
}

fn fields(prefix: &str) -> std::collections::BTreeMap<String, String> {
    line_after(prefix)
        .split(' ')
        .filter_map(|w| w.split_once('='))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn station_events_match_the_linux_parsers() {
    let ev = cmd_ok("mgmt_rx");
    let m = parse_mgmt_rx(&ev).unwrap();
    let f = fields("mgmt_rx.ret 0");
    assert_eq!(m.channel.to_string(), f["channel"]);
    assert_eq!(m.snr.to_string(), f["snr"]);
    assert_eq!(m.rate.to_string(), f["rate"]);
    assert_eq!(m.phy_mode.to_string(), f["phy_mode"]);
    assert_eq!(m.status.to_string(), f["status"]);
    assert_eq!(
        format!("{},{},{},{}", m.rssi[0], m.rssi[1], m.rssi[2], m.rssi[3]),
        f["rssi"]
    );
    assert_eq!(m.frame.len().to_string(), f["frame_len"]);
    assert_eq!(
        (m.frame.as_ptr() as usize - ev.as_ptr() as usize).to_string(),
        f["frame_off"]
    );
    assert_eq!(
        parse_mgmt_rx(&cmd_ok("mgmt_rx_long")),
        Err(WmiError::Protocol)
    );
    assert!(line_after("mgmt_rx_long.ret").ends_with("-71"));
    assert_eq!(
        parse_mgmt_rx(&cmd_ok("mgmt_rx_noframe")),
        Err(WmiError::Protocol)
    );
    assert!(line_after("mgmt_rx_noframe.ret").ends_with("-71"));

    let c = parse_mgmt_tx_compl(&cmd_ok("mgmt_tx_compl")).unwrap();
    let f = fields("mgmt_tx_compl.ret 0");
    assert_eq!(c.desc_id.to_string(), f["desc_id"]);
    assert_eq!(c.status.to_string(), f["status"]);
    assert_eq!(c.ack_rssi.to_string(), f["ack_rssi"]);

    // the fixture prints the arrays as the C parser finds them
    let b = parse_mgmt_tx_bundle_compl(&cmd_ok("mgmt_tx_bundle")).unwrap();
    assert!(line_after("mgmt_tx_bundle.ret 0").contains("num_reports=2"));
    assert!(
        line_after("mgmt_tx_bundle.ret 0").contains("id=3 status=0 ack=40 id=4 status=2 ack=41")
    );
    assert_eq!(
        b,
        vec![
            MgmtTxCompl {
                desc_id: 3,
                status: 0,
                ack_rssi: 40
            },
            MgmtTxCompl {
                desc_id: 4,
                status: 2,
                ack_rssi: 41
            }
        ]
    );

    assert_eq!(
        parse_peer_kickout(&cmd_ok("peer_kick")).unwrap(),
        [0xde, 0xad, 0xbe, 0xef, 0, 1]
    );
    let r = parse_roam(&cmd_ok("roam")).unwrap();
    let f = fields("roam.ret 0");
    assert_eq!(r.vdev_id.to_string(), f["vdev_id"]);
    assert_eq!(r.reason.to_string(), f["reason"]);
    assert_eq!(r.rssi.to_string(), f["rssi"]);
}

#[test]
fn hostile_station_events_are_refused_without_panic() {
    // a short management completion: no policy covers the tag
    let mut b = vec![4, 0, 0, 0];
    b[2..4].copy_from_slice(&tag::MGMT_TX_COMPL_EVENT.to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    assert_eq!(parse_mgmt_tx_compl(&b), Err(WmiError::Invalid));
    // a huge report count is cut to the arrays
    let mut e = Vec::new();
    let mut tlv = |t: u16, v: &[u8]| {
        e.extend_from_slice(&(v.len() as u16).to_le_bytes());
        e.extend_from_slice(&t.to_le_bytes());
        e.extend_from_slice(v);
    };
    tlv(tag::MGMT_TX_COMPL_BUNDLE_EVENT, &u32::MAX.to_le_bytes());
    tlv(tag::ARRAY_UINT32, &7u32.to_le_bytes());
    tlv(tag::ARRAY_UINT32, &1u32.to_le_bytes());
    assert_eq!(parse_mgmt_tx_bundle_compl(&e).unwrap().len(), 1);
    // random bytes through every parser
    let mut l = common::Lfsr(0x1234_5678);
    for _ in 0..3000 {
        let n = (l.next() % 120) as usize;
        let mut d = vec![0u8; n];
        l.fill(&mut d);
        if n >= 4 && l.byte() & 1 == 0 {
            // a plausible header so that the walk goes on
            d[0] = (n as u8).saturating_sub(4);
            d[1] = 0;
            let t = [
                tag::MGMT_RX_HDR,
                tag::ROAM_EVENT,
                tag::PEER_STA_KICKOUT_EVENT,
                tag::MGMT_TX_COMPL_BUNDLE_EVENT,
            ][(l.byte() & 3) as usize];
            d[2..4].copy_from_slice(&t.to_le_bytes());
        }
        let _ = parse_mgmt_rx(&d);
        let _ = parse_mgmt_tx_compl(&d);
        let _ = parse_mgmt_tx_bundle_compl(&d);
        let _ = parse_peer_kickout(&d);
        let _ = parse_roam(&d);
    }
}
