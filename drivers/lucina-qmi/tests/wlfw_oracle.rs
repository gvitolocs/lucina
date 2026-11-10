//! Byte equality with Linux's own encoder and decoder.
//!
//! `tests/fixtures/*.txt` come from `oracle/`, which compiles Linux's `qmi_encdec.c` and ath10k's
//! `qmi_wlfw_v01.c` and runs them. Every message is built here from values typed in separately from the C
//! harness, encoded, and compared byte for byte; the C bytes are then decoded and compared field for field.
//! Each case also gets truncated, grown and mutated: nothing may panic, and a cut that lands inside a TLV must
//! be an error.

use lucina_qmi::codec::{
    self, Error, HEADER_LEN, Header, INDICATION, Message, REQUEST, RESPONSE, Tlvs, decode_message,
    encode_message, encode_message_limited,
};
use lucina_qmi::wlfw::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

const ENCODE: &str = include_str!("fixtures/encode.txt");
const ERRORS: &str = include_str!("fixtures/errors.txt");
const DECODE: &str = include_str!("fixtures/decode.txt");
const CONSTANTS: &str = include_str!("fixtures/constants.txt");

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

/// `x[i] = i * 7 + 3`, the pattern the harness fills data with.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + 3) as u8).collect()
}

struct Golden {
    cases: BTreeMap<String, Vec<u8>>,
    used: RefCell<BTreeSet<String>>,
}

impl Golden {
    fn load() -> Golden {
        let mut cases = BTreeMap::new();
        for l in lines(ENCODE) {
            let (name, hex) = l.split_once(' ').unwrap();
            assert!(
                cases.insert(name.to_string(), unhex(hex)).is_none(),
                "{name} twice"
            );
        }
        Golden {
            cases,
            used: RefCell::new(BTreeSet::new()),
        }
    }

    fn all_used(&self) {
        let used = self.used.borrow();
        for name in self.cases.keys() {
            assert!(used.contains(name), "fixture {name} is not tested");
        }
    }

    /// Encode equals the C bytes, decode of the C bytes equals the message, then abuse the bytes.
    fn check<M: Message + PartialEq + Debug>(&self, case: &str, ty: u8, txn: u16, m: M) {
        let want = self
            .cases
            .get(case)
            .unwrap_or_else(|| panic!("no fixture {case}"));
        self.used.borrow_mut().insert(case.to_string());

        let got = encode_message(ty, txn, &m).unwrap_or_else(|e| panic!("{case}: encode: {e}"));
        if got != *want {
            let at = got.iter().zip(want).position(|(a, b)| a != b);
            panic!(
                "{case}: bytes differ (ours {} bytes, C {} bytes, first difference at {at:?})",
                got.len(),
                want.len()
            );
        }

        let (h, back) = decode_message::<M>(want).unwrap_or_else(|e| panic!("{case}: decode: {e}"));
        assert_eq!(back, m, "{case}");
        assert_eq!(h.msg_type, ty, "{case}");
        assert_eq!(h.txn_id, txn, "{case}");
        assert_eq!(h.msg_id, M::MSG_ID, "{case}: message id");
        assert_eq!(usize::from(h.msg_len), want.len() - HEADER_LEN, "{case}");
        assert!(
            want.len() - HEADER_LEN <= M::MAX_LEN,
            "{case}: longer than MAX_LEN"
        );

        abuse::<M>(case, want);
    }
}

/// Offsets inside `body` where a TLV ends (and the next begins), including 0 and `body.len()`.
fn tlv_boundaries(body: &[u8]) -> Vec<usize> {
    let mut v = vec![0];
    let mut pos = 0;
    while pos < body.len() {
        let len = usize::from(u16::from_le_bytes([body[pos + 1], body[pos + 2]]));
        pos += 3 + len;
        v.push(pos);
    }
    assert_eq!(pos, body.len());
    v
}

fn abuse<M: Message>(case: &str, msg: &[u8]) {
    let body = &msg[HEADER_LEN..];

    // Every cut of the whole packet is refused: too short for a header, or the length no longer matches.
    for k in 0..msg.len() {
        let r = decode_message::<M>(&msg[..k]);
        if k < HEADER_LEN {
            assert_eq!(r.err(), Some(Error::ShortHeader), "{case} cut at {k}");
        } else {
            assert_eq!(r.err(), Some(Error::LengthMismatch), "{case} cut at {k}");
        }
    }
    // A header that lies about the length, either way.
    for delta in [1i32, -1, 100, -100] {
        let mut m = msg.to_vec();
        let len = (i32::from(u16::from_le_bytes([m[5], m[6]])) + delta) as u16;
        m[5..7].copy_from_slice(&len.to_le_bytes());
        assert_eq!(
            decode_message::<M>(&m).err(),
            Some(Error::LengthMismatch),
            "{case} length {delta:+}"
        );
    }

    // The body cut at every byte with the header repaired: a cut inside a TLV is an error, a cut between
    // two TLVs is a shorter message.
    let bounds = tlv_boundaries(body);
    // Every byte of a short body; of a long one (board data) the ends, the places next to TLV boundaries and
    // a sample in between, to keep the run short.
    let interesting = |k: usize| {
        body.len() <= 600
            || k < 40
            || k + 40 > body.len()
            || k.is_multiple_of(53)
            || bounds.iter().any(|b| k.abs_diff(*b) <= 4)
    };
    for k in (0..body.len()).filter(|&k| interesting(k)) {
        let r = M::decode_body(&body[..k]);
        if !bounds.contains(&k) {
            assert_eq!(r.err(), Some(Error::Truncated), "{case} body cut at {k}");
        }
    }

    // Each TLV one and two bytes shorter and one byte longer, its length field repaired; and every byte
    // flipped, zeroed and set to 0x80. The result does not matter, only that nothing panics.
    for w in bounds.windows(2) {
        let (start, end) = (w[0], w[1]);
        for delta in [-2i32, -1, 1] {
            let vlen = (end - start - 3) as i32 + delta;
            if vlen < 0 {
                continue;
            }
            let mut b = body[..start].to_vec();
            b.push(body[start]);
            b.extend_from_slice(&(vlen as u16).to_le_bytes());
            if delta < 0 {
                b.extend_from_slice(&body[start + 3..end - (-delta) as usize]);
            } else {
                b.extend_from_slice(&body[start + 3..end]);
                b.push(0xaa);
            }
            b.extend_from_slice(&body[end..]);
            let _ = M::decode_body(&b);
        }
    }
    for i in (0..body.len()).filter(|&i| interesting(i)) {
        for v in [body[i] ^ 0xff, 0x00, 0x80, 0xff] {
            let mut b = body.to_vec();
            b[i] = v;
            let _ = M::decode_body(&b);
        }
    }
}

fn ok() -> Response {
    Response::OK
}

fn fail(error: u16) -> Response {
    Response { result: 1, error }
}

#[test]
fn constants_match_the_c_headers() {
    let mut c = BTreeMap::new();
    for l in lines(CONSTANTS) {
        let (name, v) = l.rsplit_once(' ').unwrap();
        c.insert(name.to_string(), v.parse::<u64>().unwrap());
    }
    let mut used = BTreeSet::new();
    let mut eq = |name: &str, ours: u64| {
        let theirs = *c
            .get(name)
            .unwrap_or_else(|| panic!("no C constant {name}"));
        assert_eq!(ours, theirs, "{name}");
        used.insert(name.to_string());
    };

    eq("WLFW_SERVICE_ID_V01", u64::from(SERVICE_ID));
    eq("WLFW_SERVICE_VERS_V01", u64::from(SERVICE_VERSION));
    eq("sizeof(struct qmi_header)", HEADER_LEN as u64);
    for (name, id) in [
        ("QMI_WLFW_IND_REGISTER_REQ_V01", msg_id::IND_REGISTER),
        ("QMI_WLFW_FW_READY_IND_V01", msg_id::FW_READY_IND),
        ("QMI_WLFW_WLAN_MODE_REQ_V01", msg_id::WLAN_MODE),
        ("QMI_WLFW_WLAN_CFG_REQ_V01", msg_id::WLAN_CFG),
        ("QMI_WLFW_CAP_REQ_V01", msg_id::CAP),
        ("QMI_WLFW_BDF_DOWNLOAD_REQ_V01", msg_id::BDF_DOWNLOAD),
        ("QMI_WLFW_CAL_REPORT_REQ_V01", msg_id::CAL_REPORT),
        ("QMI_WLFW_CAL_DOWNLOAD_REQ_V01", msg_id::CAL_DOWNLOAD),
        (
            "QMI_WLFW_INITIATE_CAL_DOWNLOAD_IND_V01",
            msg_id::INITIATE_CAL_DOWNLOAD_IND,
        ),
        ("QMI_WLFW_CAL_UPDATE_REQ_V01", msg_id::CAL_UPDATE),
        (
            "QMI_WLFW_INITIATE_CAL_UPDATE_IND_V01",
            msg_id::INITIATE_CAL_UPDATE_IND,
        ),
        ("QMI_WLFW_MSA_READY_IND_V01", msg_id::MSA_READY_IND),
        (
            "QMI_WLFW_PIN_CONNECT_RESULT_IND_V01",
            msg_id::PIN_CONNECT_RESULT_IND,
        ),
        ("QMI_WLFW_MSA_INFO_REQ_V01", msg_id::MSA_INFO),
        ("QMI_WLFW_MSA_READY_REQ_V01", msg_id::MSA_READY),
        ("QMI_WLFW_INI_REQ_V01", msg_id::INI),
        ("QMI_WLFW_ATHDIAG_READ_REQ_V01", msg_id::ATHDIAG_READ),
        ("QMI_WLFW_ATHDIAG_WRITE_REQ_V01", msg_id::ATHDIAG_WRITE),
        ("QMI_WLFW_VBATT_REQ_V01", msg_id::VBATT),
        ("QMI_WLFW_MAC_ADDR_REQ_V01", msg_id::MAC_ADDR),
        ("QMI_WLFW_HOST_CAP_REQ_V01", msg_id::HOST_CAP),
        ("QMI_WLFW_REQUEST_MEM_IND_V01", msg_id::REQUEST_MEM_IND),
        ("QMI_WLFW_RESPOND_MEM_REQ_V01", msg_id::RESPOND_MEM),
        ("QMI_WLFW_MEM_READY_IND_V01", msg_id::MEM_READY_IND),
        ("QMI_WLFW_FW_INIT_DONE_IND_V01", msg_id::FW_INIT_DONE_IND),
        ("QMI_WLFW_REJUVENATE_IND_V01", msg_id::REJUVENATE_IND),
        ("QMI_WLFW_REJUVENATE_ACK_REQ_V01", msg_id::REJUVENATE_ACK),
        (
            "QMI_WLFW_DYNAMIC_FEATURE_MASK_REQ_V01",
            msg_id::DYNAMIC_FEATURE_MASK,
        ),
        ("QMI_WLFW_M3_INFO_REQ_V01", msg_id::M3_INFO),
        ("QMI_WLFW_XO_CAL_IND_V01", msg_id::XO_CAL_IND),
    ] {
        eq(name, u64::from(id));
    }
    for (name, v) in [
        ("QMI_WLFW_MAX_MEM_REG_V01", MAX_MEM_REG),
        ("QMI_WLFW_MAX_NUM_CAL_V01", MAX_NUM_CAL),
        ("QMI_WLFW_MAX_DATA_SIZE_V01", MAX_DATA_SIZE),
        ("QMI_WLFW_MAX_NUM_CE_V01", MAX_NUM_CE),
        ("QMI_WLFW_MAX_TIMESTAMP_LEN_V01", MAX_TIMESTAMP_LEN),
        ("QMI_WLFW_MAX_NUM_GPIO_V01", MAX_NUM_GPIO),
        ("QMI_WLFW_MAX_BUILD_ID_LEN_V01", MAX_BUILD_ID_LEN),
        ("QMI_WLFW_MAX_STR_LEN_V01", MAX_STR_LEN),
        ("QMI_WLFW_MAX_NUM_SHADOW_REG_V01", MAX_NUM_SHADOW_REG),
        ("QMI_WLFW_MAX_SHADOW_REG_V2", MAX_SHADOW_REG_V2),
        ("QMI_WLFW_MAX_NUM_SVC_V01", MAX_NUM_SVC),
        (
            "WLFW_IND_REGISTER_REQ_MSG_V01_MAX_MSG_LEN",
            IndRegisterReq::MAX_LEN,
        ),
        (
            "WLFW_IND_REGISTER_RESP_MSG_V01_MAX_MSG_LEN",
            IndRegisterResp::MAX_LEN,
        ),
        (
            "WLFW_WLAN_MODE_REQ_MSG_V01_MAX_MSG_LEN",
            WlanModeReq::MAX_LEN,
        ),
        (
            "WLFW_WLAN_MODE_RESP_MSG_V01_MAX_MSG_LEN",
            WlanModeResp::MAX_LEN,
        ),
        ("WLFW_WLAN_CFG_REQ_MSG_V01_MAX_MSG_LEN", WlanCfgReq::MAX_LEN),
        (
            "WLFW_WLAN_CFG_RESP_MSG_V01_MAX_MSG_LEN",
            WlanCfgResp::MAX_LEN,
        ),
        ("WLFW_CAP_RESP_MSG_V01_MAX_MSG_LEN", CapResp::MAX_LEN),
        (
            "WLFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_MSG_LEN",
            BdfDownloadReq::MAX_LEN,
        ),
        (
            "WLFW_BDF_DOWNLOAD_RESP_MSG_V01_MAX_MSG_LEN",
            BdfDownloadResp::MAX_LEN,
        ),
        (
            "WLFW_CAL_REPORT_REQ_MSG_V01_MAX_MSG_LEN",
            CalReportReq::MAX_LEN,
        ),
        (
            "WLFW_CAL_REPORT_RESP_MSG_V01_MAX_MSG_LEN",
            CalReportResp::MAX_LEN,
        ),
        ("WLFW_MSA_INFO_REQ_MSG_V01_MAX_MSG_LEN", MsaInfoReq::MAX_LEN),
        (
            "WLFW_MSA_INFO_RESP_MSG_V01_MAX_MSG_LEN",
            MsaInfoResp::MAX_LEN,
        ),
        (
            "WLFW_MSA_READY_RESP_MSG_V01_MAX_MSG_LEN",
            MsaReadyResp::MAX_LEN,
        ),
        ("WLFW_INI_REQ_MSG_V01_MAX_MSG_LEN", IniReq::MAX_LEN),
        ("WLFW_INI_RESP_MSG_V01_MAX_MSG_LEN", IniResp::MAX_LEN),
        ("WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN", HostCapReq::MAX_LEN),
        (
            "WLFW_HOST_CAP_REQ_MSG_V01_MAX_MSG_LEN",
            HostCapReq8bit::MAX_LEN,
        ),
        (
            "WLFW_HOST_CAP_RESP_MSG_V01_MAX_MSG_LEN",
            HostCapResp::MAX_LEN,
        ),
    ] {
        eq(name, v as u64);
    }
    for (name, v) in [
        ("QMI_WLFW_MISSION_V01", driver_mode::MISSION),
        ("QMI_WLFW_FTM_V01", driver_mode::FTM),
        ("QMI_WLFW_EPPING_V01", driver_mode::EPPING),
        ("QMI_WLFW_WALTEST_V01", driver_mode::WALTEST),
        ("QMI_WLFW_OFF_V01", driver_mode::OFF),
        ("QMI_WLFW_CCPM_V01", driver_mode::CCPM),
        ("QMI_WLFW_QVIT_V01", driver_mode::QVIT),
        ("QMI_WLFW_CALIBRATION_V01", driver_mode::CALIBRATION),
        ("QMI_WLFW_MEM_TYPE_MSA_V01", mem_type::MSA),
        ("QMI_WLFW_MEM_TYPE_DDR_V01", mem_type::DDR),
    ] {
        eq(name, v as u64);
    }
    for (name, v) in [
        ("QMI_WLFW_PIPEDIR_NONE_V01", pipedir::NONE),
        ("QMI_WLFW_PIPEDIR_IN_V01", pipedir::IN),
        ("QMI_WLFW_PIPEDIR_OUT_V01", pipedir::OUT),
        ("QMI_WLFW_PIPEDIR_INOUT_V01", pipedir::INOUT),
        ("QMI_WLFW_CE_ATTR_NO_SNOOP_V01", ce_attr::NO_SNOOP),
        (
            "QMI_WLFW_CE_ATTR_BYTE_SWAP_DATA_V01",
            ce_attr::BYTE_SWAP_DATA,
        ),
        (
            "QMI_WLFW_CE_ATTR_SWIZZLE_DESCRIPTORS_V01",
            ce_attr::SWIZZLE_DESCRIPTORS,
        ),
        ("QMI_WLFW_CE_ATTR_DISABLE_INTR_V01", ce_attr::DISABLE_INTR),
        ("QMI_WLFW_CE_ATTR_ENABLE_POLL_V01", ce_attr::ENABLE_POLL),
    ] {
        eq(name, u64::from(v));
    }
    for (name, v) in [
        (
            "QMI_WLFW_ALREADY_REGISTERED_V01",
            fw_status::ALREADY_REGISTERED,
        ),
        ("QMI_WLFW_FW_READY_V01", fw_status::FW_READY),
        ("QMI_WLFW_MSA_READY_V01", fw_status::MSA_READY),
        ("QMI_WLFW_MEM_READY_V01", fw_status::MEM_READY),
        ("QMI_WLFW_FW_INIT_DONE_V01", fw_status::FW_INIT_DONE),
    ] {
        eq(name, v);
    }
    for (name, v) in [
        ("QMI_REQUEST", REQUEST),
        ("QMI_RESPONSE", RESPONSE),
        ("QMI_INDICATION", INDICATION),
    ] {
        eq(name, u64::from(v));
    }
    for (name, v) in [
        ("QMI_RESULT_SUCCESS_V01", codec::RESULT_SUCCESS),
        ("QMI_RESULT_FAILURE_V01", codec::RESULT_FAILURE),
        ("QMI_ERR_NONE_V01", codec::ERR_NONE),
        ("QMI_ERR_MALFORMED_MSG_V01", codec::ERR_MALFORMED_MSG),
        ("QMI_ERR_NO_MEMORY_V01", codec::ERR_NO_MEMORY),
        ("QMI_ERR_INTERNAL_V01", codec::ERR_INTERNAL),
        (
            "QMI_ERR_CLIENT_IDS_EXHAUSTED_V01",
            codec::ERR_CLIENT_IDS_EXHAUSTED,
        ),
        ("QMI_ERR_INVALID_ID_V01", codec::ERR_INVALID_ID),
        ("QMI_ERR_ENCODING_V01", codec::ERR_ENCODING),
        ("QMI_ERR_DISABLED_V01", codec::ERR_DISABLED),
        (
            "QMI_ERR_INCOMPATIBLE_STATE_V01",
            codec::ERR_INCOMPATIBLE_STATE,
        ),
        ("QMI_ERR_NOT_SUPPORTED_V01", codec::ERR_NOT_SUPPORTED),
    ] {
        eq(name, u64::from(v));
    }
    assert_eq!(ATH10K_CLIENT_ID, 0x4b4e_454c);
    assert_eq!(qrtr_instance(SERVICE_VERSION, SERVICE_INSTANCE), 1);
    for name in c.keys() {
        assert!(used.contains(name), "C constant {name} is not checked");
    }
}

fn indication_register(g: &Golden) {
    g.check(
        "ind_register_req_ath10k",
        REQUEST,
        0,
        IndRegisterReq {
            client_id: Some(0x4b4e_454c),
            fw_ready_enable: Some(1),
            msa_ready_enable: Some(1),
            ..Default::default()
        },
    );
    g.check(
        "ind_register_req_xo_cal",
        REQUEST,
        1,
        IndRegisterReq {
            client_id: Some(0x4b4e_454c),
            fw_ready_enable: Some(1),
            msa_ready_enable: Some(1),
            xo_cal_enable: Some(1),
            ..Default::default()
        },
    );
    g.check(
        "ind_register_req_all",
        REQUEST,
        2,
        IndRegisterReq {
            fw_ready_enable: Some(0x11),
            initiate_cal_download_enable: Some(0x22),
            initiate_cal_update_enable: Some(0x33),
            msa_ready_enable: Some(0x44),
            pin_connect_result_enable: Some(0x55),
            client_id: Some(0xa1b2_c3d4),
            request_mem_enable: Some(0x66),
            mem_ready_enable: Some(0x77),
            fw_init_done_enable: Some(0x88),
            rejuvenate_enable: Some(0x0102_0304),
            xo_cal_enable: Some(0x99),
        },
    );
    g.check(
        "ind_register_resp_fw_status",
        RESPONSE,
        0x0102,
        IndRegisterResp {
            resp: ok(),
            fw_status: Some(
                fw_status::FW_READY | fw_status::ALREADY_REGISTERED | 0x0123_4567_0000_0000,
            ),
        },
    );
    g.check(
        "ind_register_resp_ok",
        RESPONSE,
        0x0103,
        IndRegisterResp::default(),
    );
    g.check(
        "ind_register_resp_failure",
        RESPONSE,
        0x0104,
        IndRegisterResp {
            resp: fail(codec::ERR_INCOMPATIBLE_STATE),
            fw_status: None,
        },
    );
    g.check("fw_ready_ind", INDICATION, 0, FwReadyInd);
    g.check("msa_ready_ind", INDICATION, 0x7777, MsaReadyInd);
}

fn host_capabilities(g: &Golden) {
    g.check(
        "host_cap_req_ath10k",
        REQUEST,
        3,
        HostCapReq {
            daemon_support: Some(0),
            ..Default::default()
        },
    );
    g.check(
        "host_cap_req_8bit",
        REQUEST,
        4,
        HostCapReq8bit {
            daemon_support: Some(1),
        },
    );
    g.check(
        "host_cap_req_all",
        REQUEST,
        5,
        HostCapReq {
            daemon_support: Some(0x1122_3344),
            wake_msi: Some(0x5566_7788),
            gpios: Some(vec![0xa0a1_a2a3, 5, 0xffff_fffe]),
            nm_modem: Some(0x10),
            bdf_support: Some(0x11),
            bdf_cache_support: Some(0x12),
            m3_support: Some(0x13),
            m3_cache_support: Some(0x14),
            cal_filesys_support: Some(0x15),
            cal_cache_support: Some(0x16),
            cal_done: Some(0x17),
            mem_bucket: Some(0x0123_4567),
            mem_cfg_mode: Some(0x18),
        },
    );
    g.check(
        "host_cap_req_gpios_empty",
        REQUEST,
        6,
        HostCapReq {
            gpios: Some(vec![]),
            ..Default::default()
        },
    );
    g.check(
        "host_cap_req_gpios_32",
        REQUEST,
        7,
        HostCapReq {
            gpios: Some((0..32).map(|i| 0x100 + i).collect()),
            ..Default::default()
        },
    );
    g.check("host_cap_resp_ok", RESPONSE, 3, HostCapResp::default());
    g.check(
        "host_cap_resp_not_supported",
        RESPONSE,
        4,
        HostCapResp {
            resp: fail(codec::ERR_NOT_SUPPORTED),
        },
    );
    // The 8-bit table is a different layout of the same message: the 4-byte form must not decode it as one.
    let c8 = &g.cases["host_cap_req_8bit"];
    assert_eq!(&c8[HEADER_LEN..], &[0x10, 1, 0, 1]);
}

fn msa_and_capabilities(g: &Golden) {
    g.check(
        "msa_info_req",
        REQUEST,
        8,
        MsaInfoReq {
            msa_addr: 0x9390_0000,
            size: 0x20_0000,
        },
    );
    g.check(
        "msa_info_req_wide",
        REQUEST,
        9,
        MsaInfoReq {
            msa_addr: 0xfedc_ba98_7654_3210,
            size: 0xdead_beef,
        },
    );
    let r0 = MemoryRegionInfo {
        region_addr: 0x9390_0000,
        size: 0x10_0000,
        secure_flag: 1,
    };
    let r1 = MemoryRegionInfo {
        region_addr: 0x93a0_0000,
        size: 0x8_0000,
        secure_flag: 0,
    };
    g.check(
        "msa_info_resp_2",
        RESPONSE,
        8,
        MsaInfoResp {
            resp: ok(),
            mem_region_info: vec![r0, r1],
        },
    );
    g.check(
        "msa_info_resp_1",
        RESPONSE,
        9,
        MsaInfoResp {
            resp: ok(),
            mem_region_info: vec![r0],
        },
    );
    g.check("msa_info_resp_0", RESPONSE, 10, MsaInfoResp::default());
    g.check("msa_ready_req", REQUEST, 11, MsaReadyReq);
    g.check("msa_ready_resp_ok", RESPONSE, 11, MsaReadyResp::default());
    g.check(
        "msa_ready_resp_failure",
        RESPONSE,
        12,
        MsaReadyResp {
            resp: fail(codec::ERR_INTERNAL),
        },
    );

    g.check("cap_req", REQUEST, 13, CapReq);
    let full = CapResp {
        resp: ok(),
        chip_info: Some(RfChipInfo {
            chip_id: 0x30224,
            chip_family: 0x4001,
        }),
        board_info: Some(RfBoardInfo { board_id: 0x55 }),
        soc_info: Some(SocInfo {
            soc_id: 0x4006_0000,
        }),
        fw_version_info: Some(FwVersionInfo {
            fw_version: 0x32c8_c9bc,
            fw_build_timestamp: b"2019-03-26 13:54:20".to_vec(),
        }),
        fw_build_id: Some(b"QC_IMAGE_VERSION_STRING=WLAN.HL.3.1-00959-QCAHLSWMTPLZ-1".to_vec()),
        num_macs: Some(2),
    };
    g.check("cap_resp_full", RESPONSE, 13, full.clone());
    let mut long = full;
    long.fw_version_info.as_mut().unwrap().fw_build_timestamp = vec![b't'; 32];
    long.fw_build_id = Some(vec![b'i'; 128]);
    g.check("cap_resp_long_strings", RESPONSE, 14, long);
    g.check("cap_resp_minimal", RESPONSE, 15, CapResp::default());
    g.check(
        "cap_resp_failure",
        RESPONSE,
        16,
        CapResp {
            resp: fail(codec::ERR_MALFORMED_MSG),
            chip_info: Some(RfChipInfo {
                chip_id: 1,
                chip_family: 0,
            }),
            ..Default::default()
        },
    );
}

fn board_data_and_calibration(g: &Golden) {
    g.check(
        "bdf_req_seg0_6000",
        REQUEST,
        17,
        BdfDownloadReq {
            valid: 1,
            file_id: Some(0),
            total_size: Some(12011),
            seg_id: Some(0),
            data: Some(pattern(6000)),
            end: Some(0),
            bdf_type: None,
        },
    );
    g.check(
        "bdf_req_seg2_last_11",
        REQUEST,
        18,
        BdfDownloadReq {
            valid: 1,
            file_id: Some(0),
            total_size: Some(12011),
            seg_id: Some(2),
            data: Some(pattern(11)),
            end: Some(1),
            bdf_type: None,
        },
    );
    g.check(
        "bdf_req_max_6144_bdf_type",
        REQUEST,
        19,
        BdfDownloadReq {
            valid: 1,
            file_id: Some(3),
            total_size: Some(0xcafe),
            seg_id: Some(0x0102_0304),
            data: Some(pattern(6144)),
            end: Some(1),
            bdf_type: Some(0x7e),
        },
    );
    g.check(
        "bdf_req_data_empty",
        REQUEST,
        20,
        BdfDownloadReq {
            valid: 1,
            data: Some(vec![]),
            ..Default::default()
        },
    );
    g.check("bdf_resp_ok", RESPONSE, 17, BdfDownloadResp::default());
    g.check(
        "bdf_resp_malformed",
        RESPONSE,
        18,
        BdfDownloadResp {
            resp: fail(codec::ERR_MALFORMED_MSG),
        },
    );

    g.check("cal_report_req_empty", REQUEST, 21, CalReportReq::default());
    g.check(
        "cal_report_req_xo",
        REQUEST,
        22,
        CalReportReq {
            meta_data: vec![],
            xo_cal_data: Some(0xa5),
        },
    );
    g.check(
        "cal_report_req_3",
        REQUEST,
        23,
        CalReportReq {
            meta_data: vec![0, 2, 4],
            xo_cal_data: Some(0xa5),
        },
    );
    g.check(
        "cal_report_req_5",
        REQUEST,
        24,
        CalReportReq {
            meta_data: vec![0, 1, 2, 3, 4],
            xo_cal_data: Some(0xa5),
        },
    );
    g.check("cal_report_resp_ok", RESPONSE, 21, CalReportResp::default());
    g.check(
        "cal_report_resp_failure",
        RESPONSE,
        22,
        CalReportResp {
            resp: fail(codec::ERR_INTERNAL),
        },
    );
}

fn ath10k_tgt() -> Vec<CeTgtPipeCfg> {
    let t = |pipe_num, pipe_dir, nentries, nbytes_max, flags| CeTgtPipeCfg {
        pipe_num,
        pipe_dir,
        nentries,
        nbytes_max,
        flags,
    };
    vec![
        t(0, pipedir::OUT, 32, 2048, 0),
        t(1, pipedir::IN, 32, 2048, 0),
        t(2, pipedir::IN, 64, 2048, 0),
        t(3, pipedir::OUT, 32, 2048, 0),
        t(4, pipedir::OUT, 4096, 256, ce_attr::DISABLE_INTR),
        t(5, pipedir::OUT, 32, 2048, 0),
        t(6, pipedir::INOUT, 32, 4096, 0),
        t(7, pipedir::INOUT, 32, 2048, ce_attr::BYTE_SWAP_DATA),
        t(8, pipedir::NONE, 0, 0, 0),
        t(9, pipedir::OUT, 32, 2048, 0x1f),
    ]
}

fn ath10k_svc() -> Vec<CeSvcPipeCfg> {
    let s = |service_id, pipe_dir, pipe_num| CeSvcPipeCfg {
        service_id,
        pipe_dir,
        pipe_num,
    };
    vec![
        s(0x0000, pipedir::OUT, 3),
        s(0x0000, pipedir::IN, 2),
        s(0x0001, pipedir::OUT, 3),
        s(0x0001, pipedir::IN, 2),
        s(0x0100, pipedir::OUT, 4),
        s(0xdead_beef, pipedir::INOUT, 7),
    ]
}

fn ath10k_shadow() -> Vec<ShadowRegCfg> {
    vec![
        ShadowRegCfg {
            id: 0x0102,
            offset: 0x0304,
        },
        ShadowRegCfg {
            id: 0xfffe,
            offset: 1,
        },
        ShadowRegCfg {
            id: 7,
            offset: 0x8000,
        },
    ]
}

fn wlan_mode_config_and_log(g: &Golden) {
    g.check(
        "wlan_mode_req_mission",
        REQUEST,
        25,
        WlanModeReq {
            mode: driver_mode::MISSION,
            hw_debug: Some(0),
        },
    );
    g.check(
        "wlan_mode_req_off",
        REQUEST,
        26,
        WlanModeReq {
            mode: driver_mode::OFF,
            hw_debug: Some(0),
        },
    );
    g.check(
        "wlan_mode_req_cal_hwdebug",
        REQUEST,
        27,
        WlanModeReq {
            mode: driver_mode::CALIBRATION,
            hw_debug: Some(9),
        },
    );
    g.check(
        "wlan_mode_req_no_hwdebug",
        REQUEST,
        28,
        WlanModeReq {
            mode: driver_mode::CALIBRATION,
            hw_debug: None,
        },
    );
    g.check("wlan_mode_resp_ok", RESPONSE, 25, WlanModeResp::default());
    g.check(
        "wlan_mode_resp_failure",
        RESPONSE,
        26,
        WlanModeResp {
            resp: fail(codec::ERR_DISABLED),
        },
    );

    let base = WlanCfgReq {
        host_version: None,
        tgt_cfg: Some(ath10k_tgt()),
        svc_cfg: Some(ath10k_svc()),
        shadow_reg: Some(ath10k_shadow()),
        shadow_reg_v2: None,
    };
    g.check("wlan_cfg_req_ath10k", REQUEST, 29, base.clone());
    g.check(
        "wlan_cfg_req_version_v2",
        REQUEST,
        30,
        WlanCfgReq {
            host_version: Some(b"lucinaos-1.2.3".to_vec()),
            shadow_reg_v2: Some(vec![
                ShadowRegV2Cfg { addr: 0x0a0b_0c0d },
                ShadowRegV2Cfg { addr: 0xf0e0_d0c0 },
            ]),
            ..base
        },
    );
    g.check(
        "wlan_cfg_req_max",
        REQUEST,
        31,
        WlanCfgReq {
            host_version: Some(vec![b'v'; 16]),
            tgt_cfg: Some(
                (0..12u32)
                    .map(|i| CeTgtPipeCfg {
                        pipe_num: i,
                        pipe_dir: i % 4,
                        nentries: 100 + i,
                        nbytes_max: 200 + i,
                        flags: 1 << (i % 5),
                    })
                    .collect(),
            ),
            svc_cfg: Some(
                (0..24u32)
                    .map(|i| CeSvcPipeCfg {
                        service_id: 0x1000 + i,
                        pipe_dir: i % 4,
                        pipe_num: i,
                    })
                    .collect(),
            ),
            shadow_reg: Some(
                (0..24u16)
                    .map(|i| ShadowRegCfg {
                        id: i,
                        offset: 0x4000 + i,
                    })
                    .collect(),
            ),
            shadow_reg_v2: Some(
                (0..36u32)
                    .map(|i| ShadowRegV2Cfg {
                        addr: 0x9_0000 + 4 * i,
                    })
                    .collect(),
            ),
        },
    );
    g.check(
        "wlan_cfg_req_empty_arrays",
        REQUEST,
        32,
        WlanCfgReq {
            tgt_cfg: Some(vec![]),
            svc_cfg: Some(vec![]),
            ..Default::default()
        },
    );
    g.check("wlan_cfg_resp_ok", RESPONSE, 29, WlanCfgResp::default());
    g.check(
        "wlan_cfg_resp_failure",
        RESPONSE,
        30,
        WlanCfgResp {
            resp: fail(codec::ERR_INVALID_ID),
        },
    );

    g.check(
        "ini_req",
        REQUEST,
        33,
        IniReq {
            enablefwlog: Some(1),
        },
    );
    g.check("ini_req_empty", REQUEST, 34, IniReq::default());
    g.check("ini_resp_ok", RESPONSE, 33, IniResp::default());

    // The maximum message really is as long as the C says.
    let max = &g.cases["wlan_cfg_req_max"];
    assert_eq!(max.len() - HEADER_LEN, WlanCfgReq::MAX_LEN);
}

#[test]
fn golden_bytes_match_linux() {
    let g = Golden::load();
    indication_register(&g);
    host_capabilities(&g);
    msa_and_capabilities(&g);
    board_data_and_calibration(&g);
    wlan_mode_config_and_log(&g);
    g.all_used();
}

#[test]
fn the_c_refuses_what_we_refuse() {
    let mut cases = BTreeSet::new();
    let mut run = |name: &str, want_errno: i32, got: Result<Vec<u8>, Error>| {
        let rc: i32 = ERRORS
            .lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
            .unwrap_or_else(|| panic!("no fixture {name}"));
        assert_eq!(rc, want_errno, "{name}: C errno");
        // -22 EINVAL: a table longer than its maximum; -525 ETOOSMALL: the buffer given to the encoder.
        let want = match rc {
            -22 => Error::ArrayTooLong,
            -525 => Error::BufferTooSmall,
            _ => panic!("{name}: unexpected errno {rc}"),
        };
        assert_eq!(got.err(), Some(want), "{name}");
        cases.insert(name.to_string());
    };

    run(
        "ind_register_req_all_len_10",
        -525,
        encode_message_limited(
            REQUEST,
            0,
            &IndRegisterReq {
                fw_ready_enable: Some(0x11),
                initiate_cal_download_enable: Some(0x22),
                initiate_cal_update_enable: Some(0x33),
                msa_ready_enable: Some(0x44),
                pin_connect_result_enable: Some(0x55),
                client_id: Some(0xa1b2_c3d4),
                request_mem_enable: Some(0x66),
                mem_ready_enable: Some(0x77),
                fw_init_done_enable: Some(0x88),
                rejuvenate_enable: Some(0x0102_0304),
                xo_cal_enable: Some(0x99),
            },
            10,
        ),
    );
    run(
        "host_cap_req_gpios_33",
        -22,
        encode_message(
            REQUEST,
            0,
            &HostCapReq {
                gpios: Some(vec![0; 33]),
                ..Default::default()
            },
        ),
    );
    run(
        "msa_info_resp_3",
        -22,
        encode_message(
            RESPONSE,
            0,
            &MsaInfoResp {
                resp: ok(),
                mem_region_info: vec![MemoryRegionInfo::default(); 3],
            },
        ),
    );
    run(
        "bdf_req_data_6145",
        -22,
        encode_message(
            REQUEST,
            0,
            &BdfDownloadReq {
                valid: 1,
                data: Some(vec![0; 6145]),
                ..Default::default()
            },
        ),
    );
    run(
        "cal_report_req_6",
        -22,
        encode_message(
            REQUEST,
            0,
            &CalReportReq {
                meta_data: vec![0; 6],
                xo_cal_data: Some(0xa5),
            },
        ),
    );
    run(
        "wlan_cfg_req_tgt_13",
        -22,
        encode_message(
            REQUEST,
            0,
            &WlanCfgReq {
                tgt_cfg: Some(vec![CeTgtPipeCfg::default(); 13]),
                svc_cfg: Some(vec![]),
                ..Default::default()
            },
        ),
    );
    run(
        "wlan_cfg_req_tgt_12_len_240",
        -525,
        encode_message_limited(
            REQUEST,
            0,
            &WlanCfgReq {
                tgt_cfg: Some(vec![CeTgtPipeCfg::default(); 12]),
                svc_cfg: Some(vec![]),
                ..Default::default()
            },
            240,
        ),
    );
    let all: BTreeSet<String> = lines(ERRORS)
        .map(|l| l.split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(cases, all);

    // Strings: more than the buffer holds is refused on the way out too.
    let too_long = CapResp {
        fw_build_id: Some(vec![b'x'; 129]),
        ..Default::default()
    };
    assert_eq!(
        encode_message(RESPONSE, 0, &too_long).err(),
        Some(Error::StringTooLong)
    );
    let too_long = WlanCfgReq {
        host_version: Some(vec![b'x'; 17]),
        ..Default::default()
    };
    assert_eq!(
        encode_message(REQUEST, 0, &too_long).err(),
        Some(Error::StringTooLong)
    );
}

/// What the harness's `show_*` functions print, from the decoded Rust struct.
fn show_cap(m: &CapResp) -> String {
    let (fwv_valid, fwv, ts_len) = match &m.fw_version_info {
        Some(v) => (1, v.fw_version, v.fw_build_timestamp.len()),
        None => (0, 0, 0),
    };
    format!(
        " result={} num_macs_valid={} num_macs={} build_id_valid={} build_id_len={} fwv_valid={} fwv={} ts_len={}",
        m.resp.result,
        u8::from(m.num_macs.is_some()),
        m.num_macs.unwrap_or(0),
        u8::from(m.fw_build_id.is_some()),
        m.fw_build_id.as_ref().map_or(0, Vec::len),
        fwv_valid,
        fwv,
        ts_len
    )
}

fn show_msa(m: &MsaInfoResp) -> String {
    format!(" result={} len={}", m.resp.result, m.mem_region_info.len())
}

fn show_bdf(m: &BdfDownloadReq) -> String {
    format!(
        " valid={} data_valid={} data_len={} end_valid={} end={}",
        m.valid,
        u8::from(m.data.is_some()),
        m.data.as_ref().map_or(0, Vec::len),
        u8::from(m.end.is_some()),
        m.end.unwrap_or(0)
    )
}

/// Decode `bytes` as `M` and compare with the C: an error where it returned a negative number, and the same
/// printed fields where it succeeded.
fn differential<M: Message>(
    name: &str,
    bytes: &[u8],
    rc: i32,
    tail: &str,
    show: fn(&M) -> String,
    want_err: Option<Error>,
) {
    let r = decode_message::<M>(bytes);
    if rc < 0 {
        assert_eq!(r.err(), want_err, "{name}: the C returned {rc}");
    } else {
        let (_, m) = r.unwrap_or_else(|e| panic!("{name}: the C accepted it, we refuse: {e}"));
        assert_eq!(show(&m), tail, "{name}");
    }
}

#[test]
fn decoding_agrees_with_the_c_on_odd_input() {
    let mut seen = BTreeSet::new();
    for l in lines(DECODE) {
        let (name, rest) = l.split_once(' ').unwrap();
        let (hex, rest) = rest.split_once(' ').unwrap();
        let rest = rest.strip_prefix("rc=").unwrap();
        let (rc, tail) = match rest.split_once(' ') {
            Some((rc, tail)) => (rc, format!(" {tail}")),
            None => (rest, String::new()),
        };
        let rc: i32 = rc.parse().unwrap();
        let bytes = unhex(hex);
        seen.insert(name.to_string());
        match name {
            "cap_unknown_optional_skipped"
            | "cap_duplicate_last_wins"
            | "cap_build_id_128_ok"
            | "cap_timestamp_32_ok" => differential(name, &bytes, rc, &tail, show_cap, None),
            "cap_unknown_mandatory" => differential(
                name,
                &bytes,
                rc,
                &tail,
                show_cap,
                Some(Error::UnknownMandatoryTlv(5)),
            ),
            // The C accepts a response whose struct stops after the first field; we want all of it.
            "cap_response_without_error_field" => {
                assert!(rc >= 0, "the C takes it");
                assert_eq!(
                    decode_message::<CapResp>(&bytes).err(),
                    Some(Error::Truncated)
                );
            }
            "cap_build_id_129_refused" | "cap_timestamp_33_refused" => differential(
                name,
                &bytes,
                rc,
                &tail,
                show_cap,
                Some(Error::StringTooLong),
            ),
            "msa_info_resp_len_3_refused" => {
                differential(name, &bytes, rc, &tail, show_msa, Some(Error::ArrayTooLong))
            }
            "msa_info_resp_len_0_ok" => differential(name, &bytes, rc, &tail, show_msa, None),
            "bdf_req_data_6144_ok" => differential(name, &bytes, rc, &tail, show_bdf, None),
            "bdf_req_data_6145_refused" => {
                differential(name, &bytes, rc, &tail, show_bdf, Some(Error::ArrayTooLong))
            }
            _ => panic!("no test for decode fixture {name}"),
        }
    }
    assert_eq!(seen.len(), 12);
}

#[test]
fn header_round_trip() {
    let h = Header {
        msg_type: INDICATION,
        txn_id: 0xbeef,
        msg_id: 0x002b,
        msg_len: 0x0304,
    };
    let b = h.to_bytes();
    assert_eq!(b, [4, 0xef, 0xbe, 0x2b, 0x00, 0x04, 0x03]);
    let mut v = b.to_vec();
    v.extend_from_slice(&[0; 0x304]);
    let (back, body) = Header::parse(&v).unwrap();
    assert_eq!(back, h);
    assert_eq!(body.len(), 0x304);
    assert_eq!(Header::parse(&b[..6]).err(), Some(Error::ShortHeader));
}

#[test]
fn tlv_walker_stops_at_the_first_error() {
    // Three bytes of header but the value is longer than what is left.
    let mut it = Tlvs::new(&[0x10, 5, 0, 1, 2]);
    assert_eq!(it.next().unwrap().err(), Some(Error::Truncated));
    assert!(it.next().is_none());
    // A header cut short.
    let mut it = Tlvs::new(&[0x10, 5]);
    assert_eq!(it.next().unwrap().err(), Some(Error::Truncated));
    assert!(it.next().is_none());
    // Two good ones.
    let ok = [0x10, 1, 0, 9, 0x11, 0, 0];
    let got: Vec<u8> = Tlvs::new(&ok).map(|t| t.unwrap().0).collect();
    assert_eq!(got, [0x10, 0x11]);
}

#[test]
fn random_garbage_never_panics() {
    // xorshift64*, fixed seed
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        s.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let g = Golden::load();
    let seeds: Vec<&Vec<u8>> = g.cases.values().collect();
    for round in 0..3000 {
        let base = seeds[(next() % seeds.len() as u64) as usize];
        let mut b = base.clone();
        for _ in 0..(1 + next() % 4) {
            let i = (next() % b.len() as u64) as usize;
            b[i] = next() as u8;
        }
        if round % 3 == 0 {
            let cut = (next() % b.len() as u64) as usize;
            b.truncate(cut);
        }
        // Every message type, whatever the id says.
        let body = b.get(HEADER_LEN..).unwrap_or(&[]);
        let _ = IndRegisterReq::decode_body(body);
        let _ = IndRegisterResp::decode_body(body);
        let _ = HostCapReq::decode_body(body);
        let _ = HostCapReq8bit::decode_body(body);
        let _ = MsaInfoReq::decode_body(body);
        let _ = MsaInfoResp::decode_body(body);
        let _ = CapResp::decode_body(body);
        let _ = BdfDownloadReq::decode_body(body);
        let _ = CalReportReq::decode_body(body);
        let _ = WlanModeReq::decode_body(body);
        let _ = WlanCfgReq::decode_body(body);
        let _ = IniReq::decode_body(body);
        let _ = FwReadyInd::decode_body(body);
        let _ = decode_message::<WlanCfgResp>(&b);
    }
}
