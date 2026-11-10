//! The WLFW client against a scripted firmware. The requests it sends are decoded back with the codec that the
//! oracle tests tie to Linux, so a wrong field or order shows up here as a wrong struct.

use lucina_qmi::client::*;
use lucina_qmi::codec::{
    self, Error, HEADER_LEN, Header, INDICATION, Message, REQUEST, RESPONSE, decode_message,
    encode_message,
};
use lucina_qmi::wlfw::*;

const MSA_PHYS: u64 = 0x9390_0000;
const MSA_SIZE: u32 = 0x20_0000;

fn config() -> Config {
    Config::new(MSA_PHYS, MSA_SIZE)
}

fn resp<M: Message>(txn: u16, m: &M) -> Vec<u8> {
    encode_message(RESPONSE, txn, m).unwrap()
}

fn ind<M: Message>(m: &M) -> Vec<u8> {
    encode_message(INDICATION, 0, m).unwrap()
}

/// The one request in `out`, decoded as `M`; panics if there is not exactly one `Send`.
fn request<M: Message>(out: &[Output]) -> (u16, M) {
    let sends: Vec<&Vec<u8>> = out
        .iter()
        .filter_map(|o| match o {
            Output::Send(b) => Some(b),
            _ => None,
        })
        .collect();
    assert_eq!(sends.len(), 1, "expected one request in {out:?}");
    let (h, m) = decode_message::<M>(sends[0]).unwrap();
    assert_eq!(h.msg_type, REQUEST);
    assert_eq!(h.msg_id, M::MSG_ID);
    (h.txn_id, m)
}

fn facts(out: &[Output]) -> Vec<&Fact> {
    out.iter()
        .filter_map(|o| match o {
            Output::Fact(f) => Some(f),
            _ => None,
        })
        .collect()
}

fn failed(out: &[Output]) -> Option<Failure> {
    out.iter().find_map(|o| match o {
        Output::Failed(f) => Some(*f),
        _ => None,
    })
}

fn no_send(out: &[Output]) -> bool {
    !out.iter().any(|o| matches!(o, Output::Send(_)))
}

struct Run {
    c: WlfwClient,
}

impl Run {
    fn new(cfg: Config) -> Run {
        Run {
            c: WlfwClient::new(cfg),
        }
    }

    fn go(&mut self, input: Input<'_>) -> Vec<Output> {
        self.c.handle(input)
    }

    fn msg(&mut self, bytes: &[u8]) -> Vec<Output> {
        self.c.handle(Input::Message(bytes))
    }

    /// Answer the request in `out` with `m`.
    fn answer<Q: Message, A: Message>(&mut self, out: &[Output], m: &A) -> Vec<Output> {
        let (txn, _q) = request::<Q>(out);
        self.msg(&resp(txn, m))
    }

    /// ServerArrive, then answer indication register with the given firmware status.
    fn registered(&mut self, status: Option<u64>) -> Vec<Output> {
        let out = self.go(Input::ServerArrive);
        self.answer::<IndRegisterReq, _>(
            &out,
            &IndRegisterResp {
                resp: Response::OK,
                fw_status: status,
            },
        )
    }
}

fn region(addr: u64, size: u32, secure_flag: u8) -> MemoryRegionInfo {
    MemoryRegionInfo {
        region_addr: addr,
        size,
        secure_flag,
    }
}

fn msa_info_resp(regions: Vec<MemoryRegionInfo>) -> MsaInfoResp {
    MsaInfoResp {
        resp: Response::OK,
        mem_region_info: regions,
    }
}

fn cap_resp() -> CapResp {
    CapResp {
        resp: Response::OK,
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
        fw_build_id: Some(b"QC_IMAGE_VERSION_STRING=WLAN.HL.3.1".to_vec()),
        num_macs: Some(2),
    }
}

fn fail(error: u16) -> Response {
    Response { result: 1, error }
}

/// Run the server-arrive work with the firmware agreeing to everything; leaves the machine waiting for
/// MSA_READY (or, with `no_msa_ready_indicator`, for the board data). Returns the last output.
fn arrive_ok(r: &mut Run) -> Vec<Output> {
    let out = r.registered(None);
    assert!(matches!(
        facts(&out)[..],
        [Fact::IndicationsRegistered { fw_ready: false }]
    ));
    let mut out = out;
    if !r.c.config().skip_host_cap {
        if r.c.config().host_cap_8bit {
            out = r.answer::<HostCapReq8bit, _>(&out, &HostCapResp::default());
        } else {
            out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
        }
    }
    out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![region(MSA_PHYS, 0x10_0000, 0)]));
    if !r.c.config().msa_fixed_perm {
        assert!(matches!(out.last(), Some(Output::AssignMsa(_))));
        out = r.go(Input::MsaPermissions(true));
    }
    out = r.answer::<MsaReadyReq, _>(&out, &MsaReadyResp::default());
    r.answer::<CapReq, _>(&out, &cap_resp())
}

#[test]
fn the_whole_bring_up() {
    let mut r = Run::new(config());
    assert_eq!(r.c.state(), State::Idle);

    // indication register: client id, FW_READY and MSA_READY
    let out = r.go(Input::ServerArrive);
    let (txn, q) = request::<IndRegisterReq>(&out);
    assert_eq!(txn, 0, "idr_alloc_cyclic starts at 0");
    assert_eq!(
        q,
        IndRegisterReq {
            client_id: Some(0x4b4e_454c),
            fw_ready_enable: Some(1),
            msa_ready_enable: Some(1),
            ..Default::default()
        }
    );
    assert_eq!(r.c.state(), State::Awaiting(Step::IndRegister));

    // host capabilities: daemon_support 0
    let out = r.msg(&resp(
        0,
        &IndRegisterResp {
            resp: Response::OK,
            fw_status: Some(fw_status::ALREADY_REGISTERED),
        },
    ));
    assert_eq!(
        facts(&out),
        [&Fact::IndicationsRegistered { fw_ready: false }]
    );
    let (txn, q) = request::<HostCapReq>(&out);
    assert_eq!(txn, 1);
    assert_eq!(
        q,
        HostCapReq {
            daemon_support: Some(0),
            ..Default::default()
        }
    );

    // MSA info: the host's region
    let out = r.msg(&resp(1, &HostCapResp::default()));
    assert_eq!(facts(&out), [&Fact::HostCapDone { unsupported: false }]);
    let (txn, q) = request::<MsaInfoReq>(&out);
    assert_eq!(txn, 2);
    assert_eq!(
        q,
        MsaInfoReq {
            msa_addr: MSA_PHYS,
            size: MSA_SIZE
        }
    );

    // two regions inside it; the pause, then the permission step
    let regs = vec![
        region(MSA_PHYS, 0x10_0000, 1),
        region(MSA_PHYS + 0x10_0000, 0x10_0000, 0),
    ];
    let out = r.msg(&resp(2, &msa_info_resp(regs)));
    let want = vec![
        MsaRegion {
            addr: MSA_PHYS,
            size: 0x10_0000,
            secure: 1,
        },
        MsaRegion {
            addr: MSA_PHYS + 0x10_0000,
            size: 0x10_0000,
            secure: 0,
        },
    ];
    assert_eq!(
        out,
        [
            Output::Fact(Fact::MsaRegions(want.clone())),
            Output::Pause { ms: 20 },
            Output::AssignMsa(want.clone()),
        ]
    );
    assert_eq!(r.c.state(), State::AwaitingMsaPermissions);
    assert_eq!(r.c.msa_regions(), &want[..]);

    // MSA ready, then capabilities
    let out = r.go(Input::MsaPermissions(true));
    let (txn, _) = request::<MsaReadyReq>(&out);
    assert_eq!(txn, 3);
    let out = r.msg(&resp(3, &MsaReadyResp::default()));
    assert_eq!(facts(&out), [&Fact::MsaReadyAcked { accepted: true }]);
    let (txn, _) = request::<CapReq>(&out);
    assert_eq!(txn, 4);
    let out = r.msg(&resp(4, &cap_resp()));
    let caps = Capabilities {
        chip_id: 0x30224,
        chip_family: 0x4001,
        board_id: 0x55,
        soc_id: 0x4006_0000,
        fw_version: 0x32c8_c9bc,
        fw_build_timestamp: b"2019-03-26 13:54:20".to_vec(),
        fw_build_id: b"QC_IMAGE_VERSION_STRING=WLAN.HL.3.1".to_vec(),
        num_macs: Some(2),
    };
    assert_eq!(out, [Output::Fact(Fact::Capabilities(caps.clone()))]);
    assert_eq!(r.c.capabilities(), &caps);
    assert_eq!(r.c.state(), State::AwaitingMsaReady);

    // MSA_READY indication: the board data is asked for with the ids the firmware gave
    let out = r.msg(&ind(&MsaReadyInd));
    assert_eq!(
        out,
        [
            Output::Fact(Fact::MsaReadyIndication),
            Output::NeedBoardData {
                board_id: 0x55,
                chip_id: 0x30224
            }
        ]
    );
    assert_eq!(r.c.state(), State::AwaitingBoardData);

    // 14000 bytes: 6144 + 6144 + 1712, the last one flagged
    let data: Vec<u8> = (0..14000).map(|i| (i % 251) as u8).collect();
    let mut out = r.go(Input::BoardData(data.clone()));
    for (seg, range) in [(0u32, 0..6144), (1, 6144..12288), (2, 12288..14000)] {
        let (txn, q) = request::<BdfDownloadReq>(&out);
        assert_eq!(txn, 5 + seg as u16);
        assert_eq!(
            q,
            BdfDownloadReq {
                valid: 1,
                file_id: Some(0),
                total_size: Some(14000),
                seg_id: Some(seg),
                data: Some(data[range.clone()].to_vec()),
                end: Some(u8::from(seg == 2)),
                bdf_type: None,
            }
        );
        out = r.msg(&resp(txn, &BdfDownloadResp::default()));
    }
    assert_eq!(
        facts(&out),
        [&Fact::BdfDownloaded {
            chunks: 3,
            crc_rejected: false
        }]
    );
    let (txn, q) = request::<CalReportReq>(&out);
    assert_eq!(txn, 8);
    assert_eq!(q, CalReportReq::default());
    let out = r.msg(&resp(8, &CalReportResp::default()));
    assert_eq!(facts(&out), [&Fact::CalReported { accepted: true }]);
    assert_eq!(r.c.state(), State::AwaitingFwReady);

    // FW_READY
    let out = r.msg(&ind(&FwReadyInd));
    assert_eq!(out, [Output::Fact(Fact::FwReady)]);
    assert_eq!(r.c.state(), State::Ready);
    assert!(r.c.is_fw_ready());

    // wlan_enable with more entries than the message holds
    let cfg = WlanEnableCfg {
        ce_tgt_cfg: (0..14)
            .map(|i| CeTgtPipeCfg {
                pipe_num: i,
                ..Default::default()
            })
            .collect(),
        ce_svc_cfg: (0..30)
            .map(|i| CeSvcPipeCfg {
                service_id: i,
                ..Default::default()
            })
            .collect(),
        shadow_reg_cfg: (0..30)
            .map(|i| ShadowRegCfg {
                id: i,
                offset: 2 * i,
            })
            .collect(),
    };
    let out = r.go(Input::WlanEnable {
        cfg,
        mode: driver_mode::MISSION,
    });
    let (txn, q) = request::<WlanCfgReq>(&out);
    assert_eq!(txn, 9);
    assert_eq!(q.host_version, None);
    assert_eq!(q.shadow_reg_v2, None);
    assert_eq!(q.tgt_cfg.as_ref().unwrap().len(), 12);
    assert_eq!(q.tgt_cfg.as_ref().unwrap()[11].pipe_num, 11);
    assert_eq!(q.svc_cfg.as_ref().unwrap().len(), 24);
    assert_eq!(q.shadow_reg.as_ref().unwrap().len(), 24);
    let out = r.msg(&resp(txn, &WlanCfgResp::default()));
    assert_eq!(facts(&out), [&Fact::WlanConfigured]);
    let (txn, q) = request::<WlanModeReq>(&out);
    assert_eq!(txn, 10);
    assert_eq!(
        q,
        WlanModeReq {
            mode: driver_mode::MISSION,
            hw_debug: Some(0)
        }
    );
    let out = r.msg(&resp(txn, &WlanModeResp::default()));
    assert_eq!(
        out,
        [Output::Fact(Fact::ModeSet {
            mode: driver_mode::MISSION
        })]
    );
    assert_eq!(r.c.state(), State::Ready);

    // wlan_disable is the mode request alone
    let out = r.go(Input::WlanDisable);
    let (txn, q) = request::<WlanModeReq>(&out);
    assert_eq!(q.mode, driver_mode::OFF);
    let out = r.msg(&resp(txn, &WlanModeResp::default()));
    assert_eq!(
        out,
        [Output::Fact(Fact::ModeSet {
            mode: driver_mode::OFF
        })]
    );

    // firmware log mode
    let out = r.go(Input::SetFwLogMode(1));
    let (txn, q) = request::<IniReq>(&out);
    assert_eq!(q.enablefwlog, Some(1));
    let out = r.msg(&resp(txn, &IniResp::default()));
    assert_eq!(out, [Output::Fact(Fact::FwLogModeSet { mode: 1 })]);

    // the server goes away: everything is undone
    let out = r.go(Input::ServerExit);
    assert_eq!(
        out,
        [Output::UnassignMsa(want), Output::Fact(Fact::ServerGone)]
    );
    assert_eq!(r.c.state(), State::Idle);
    assert!(!r.c.is_fw_ready());
}

#[test]
fn fixed_permissions_skip_the_assignment() {
    let mut cfg = config();
    cfg.msa_fixed_perm = true;
    let mut r = Run::new(cfg);
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![region(MSA_PHYS, MSA_SIZE, 0)]));
    assert!(
        !out.iter().any(|o| matches!(o, Output::AssignMsa(_))),
        "{out:?}"
    );
    assert!(matches!(out[2], Output::Send(_)), "{out:?}");
    let (_, _) = request::<MsaReadyReq>(&out);
    // and nothing to hand back later
    let out = r.answer::<MsaReadyReq, _>(&out, &MsaReadyResp::default());
    let out = r.answer::<CapReq, _>(&out, &fail_cap(codec::ERR_INTERNAL));
    assert_eq!(
        failed(&out),
        Some(Failure::Rejected {
            step: Step::Cap,
            error: codec::ERR_INTERNAL
        })
    );
    assert!(!out.iter().any(|o| matches!(o, Output::UnassignMsa(_))));
    assert_eq!(r.c.state(), State::Failed);
}

fn fail_cap(error: u16) -> CapResp {
    CapResp {
        resp: fail(error),
        ..Default::default()
    }
}

#[test]
fn a_failure_after_the_assignment_hands_the_regions_back() {
    for cap_fails in [true, false] {
        let mut r = Run::new(config());
        let out = r.registered(None);
        let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
        let out =
            r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![region(MSA_PHYS, 0x1000, 0)]));
        assert!(matches!(out.last(), Some(Output::AssignMsa(_))));
        let out = r.go(Input::MsaPermissions(true));
        if cap_fails {
            let out = r.answer::<MsaReadyReq, _>(&out, &MsaReadyResp::default());
            let out = r.answer::<CapReq, _>(&out, &fail_cap(codec::ERR_INTERNAL));
            assert!(
                matches!(&out[0], Output::UnassignMsa(v) if v.len() == 1),
                "{out:?}"
            );
            assert!(failed(&out).is_some());
        } else {
            // the MSA-ready request times out
            let out = r.go(Input::Timeout);
            assert!(matches!(&out[0], Output::UnassignMsa(_)), "{out:?}");
            assert_eq!(
                failed(&out),
                Some(Failure::Timeout {
                    step: Step::MsaReady
                })
            );
        }
        assert_eq!(r.c.state(), State::Failed);
        // a new arrival starts over cleanly
        let out = r.go(Input::ServerArrive);
        let (_, _) = request::<IndRegisterReq>(&out);
    }
}

#[test]
fn a_rejected_msa_ready_is_only_logged() {
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![]));
    // no regions: nothing to assign, straight to MSA ready
    assert!(matches!(out.last(), Some(Output::Send(_))), "{out:?}");
    let out = r.answer::<MsaReadyReq, _>(
        &out,
        &MsaReadyResp {
            resp: fail(codec::ERR_INTERNAL),
        },
    );
    assert_eq!(facts(&out), [&Fact::MsaReadyAcked { accepted: false }]);
    let (_, _) = request::<CapReq>(&out);
}

#[test]
fn already_ready_firmware_stops_after_registration() {
    let mut r = Run::new(config());
    let out = r.registered(Some(fw_status::FW_READY));
    assert_eq!(
        out,
        [
            Output::Fact(Fact::IndicationsRegistered { fw_ready: true }),
            Output::Fact(Fact::FwReady)
        ]
    );
    assert!(no_send(&out));
    assert_eq!(r.c.state(), State::Ready);
    assert!(r.c.is_fw_ready());

    // other bits of fw_status alone do not count
    let mut r = Run::new(config());
    let out = r.registered(Some(fw_status::MSA_READY | fw_status::MEM_READY));
    assert!(!r.c.is_fw_ready());
    let (_, _) = request::<HostCapReq>(&out);
}

#[test]
fn host_capabilities_quirks_and_failures() {
    // older firmware: NOT_SUPPORTED is fine
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(
        &out,
        &HostCapResp {
            resp: fail(codec::ERR_NOT_SUPPORTED),
        },
    );
    assert_eq!(facts(&out), [&Fact::HostCapDone { unsupported: true }]);
    let (_, _) = request::<MsaInfoReq>(&out);

    // any other refusal is not
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(
        &out,
        &HostCapResp {
            resp: fail(codec::ERR_MALFORMED_MSG),
        },
    );
    assert_eq!(
        failed(&out),
        Some(Failure::Rejected {
            step: Step::HostCap,
            error: codec::ERR_MALFORMED_MSG
        })
    );

    // ATH10K_SNOC_FLAG_SKIP_HOST_CAP_QUIRK
    let mut cfg = config();
    cfg.skip_host_cap = true;
    let mut r = Run::new(cfg);
    let out = r.registered(None);
    let (_, _) = request::<MsaInfoReq>(&out);

    // ATH10K_SNOC_FLAG_8BIT_HOST_CAP_QUIRK: the one-byte table, 10 01 00 on the wire
    let mut cfg = config();
    cfg.host_cap_8bit = true;
    let mut r = Run::new(cfg);
    let out = r.registered(None);
    let Output::Send(bytes) = &out[1] else {
        panic!("{out:?}")
    };
    assert_eq!(&bytes[HEADER_LEN..], &[0x10, 1, 0, 0]);
    let (_, q) = request::<HostCapReq8bit>(&out);
    assert_eq!(q.daemon_support, Some(0));
}

#[test]
fn msa_regions_outside_the_offered_memory_are_refused() {
    let end = MSA_PHYS + u64::from(MSA_SIZE);
    for (addr, size) in [
        (MSA_PHYS - 0x1000, 0x1000),     // below the base
        (end + 1, 0x10),                 // above the end
        (MSA_PHYS, MSA_SIZE + 1),        // bigger than the whole
        (end - 0x1000, 0x2000),          // runs past the end
        (0xffff_ffff_ffff_f000, 0x2000), // wraps in 64 bits
    ] {
        let mut r = Run::new(config());
        let out = r.registered(None);
        let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
        let out = r.answer::<MsaInfoReq, _>(
            &out,
            &msa_info_resp(vec![region(MSA_PHYS, 0x1000, 0), region(addr, size, 0)]),
        );
        assert_eq!(
            failed(&out),
            Some(Failure::MsaRegionOutOfRange { addr, size }),
            "{addr:#x}+{size:#x}"
        );
        assert!(!out.iter().any(|o| matches!(o, Output::AssignMsa(_))));
        assert_eq!(r.c.state(), State::Failed);
        assert!(r.c.msa_regions().is_empty());
    }
    // exactly the whole memory, and a region ending on the last byte, are fine
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![region(MSA_PHYS, MSA_SIZE, 0)]));
    assert!(matches!(out.last(), Some(Output::AssignMsa(_))));
}

#[test]
fn the_permission_step_can_fail() {
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let _ = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![region(MSA_PHYS, 0x1000, 0)]));
    let out = r.go(Input::MsaPermissions(false));
    assert_eq!(failed(&out), Some(Failure::MsaPermissions));
    assert_eq!(r.c.state(), State::Failed);
    // an answer nobody asked for does nothing
    assert!(r.go(Input::MsaPermissions(true)).is_empty());
}

#[test]
fn without_an_msa_ready_indicator_the_board_data_follows_at_once() {
    let mut cfg = config();
    cfg.no_msa_ready_indicator = true;
    cfg.msa_fixed_perm = true;
    let mut r = Run::new(cfg);
    let out = arrive_ok(&mut r);
    assert_eq!(
        out.last(),
        Some(&Output::NeedBoardData {
            board_id: 0x55,
            chip_id: 0x30224
        })
    );
    assert_eq!(r.c.state(), State::AwaitingBoardData);
}

#[test]
fn an_unexpected_msa_ready_is_noted_and_ignored() {
    let mut cfg = config();
    cfg.no_msa_ready_indicator = true;
    cfg.msa_fixed_perm = true;
    let mut r = Run::new(cfg);
    let _ = arrive_ok(&mut r);
    let out = r.go(Input::BoardDataUnavailable);
    assert_eq!(failed(&out), Some(Failure::BoardData));
    // not in a state to care
    assert!(r.msg(&ind(&MsaReadyInd)).is_empty());

    let mut cfg = config();
    cfg.no_msa_ready_indicator = true;
    cfg.msa_fixed_perm = true;
    let mut r = Run::new(cfg);
    let _ = arrive_ok(&mut r);
    let out = r.go(Input::BoardData(vec![]));
    let _ = r.answer::<CalReportReq, _>(&out, &CalReportResp::default());
    assert_eq!(r.c.state(), State::AwaitingFwReady);
    assert_eq!(
        r.msg(&ind(&MsaReadyInd)),
        [Output::Fact(Fact::UnexpectedMsaReady)]
    );
}

#[test]
fn indications_wait_for_the_running_work_item() {
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![region(MSA_PHYS, 0x1000, 0)]));
    assert!(matches!(out.last(), Some(Output::AssignMsa(_))));
    let out = r.go(Input::MsaPermissions(true));
    let out = r.answer::<MsaReadyReq, _>(&out, &MsaReadyResp::default());
    // both indications arrive while the capability request is out
    assert!(r.msg(&ind(&FwReadyInd)).is_empty());
    assert!(r.msg(&ind(&MsaReadyInd)).is_empty());
    assert!(!r.c.is_fw_ready());
    let out = r.answer::<CapReq, _>(&out, &cap_resp());
    // after the work item, in arrival order
    assert!(matches!(
        &out[..],
        [
            Output::Fact(Fact::Capabilities(_)),
            Output::Fact(Fact::FwReady),
            Output::Fact(Fact::MsaReadyIndication),
            Output::NeedBoardData { .. }
        ]
    ));
    assert!(r.c.is_fw_ready());
    assert_eq!(r.c.state(), State::AwaitingBoardData);
}

fn at_board_data() -> Run {
    let mut r = Run::new(config());
    let _ = arrive_ok(&mut r);
    let out = r.msg(&ind(&MsaReadyInd));
    assert!(matches!(out.last(), Some(Output::NeedBoardData { .. })));
    r
}

#[test]
fn board_data_edge_cases() {
    // exactly one full chunk is also the last
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![7; 6144]));
    let (txn, q) = request::<BdfDownloadReq>(&out);
    assert_eq!(q.data.as_ref().unwrap().len(), 6144);
    assert_eq!(q.end, Some(1));
    assert_eq!(q.seg_id, Some(0));
    let out = r.msg(&resp(txn, &BdfDownloadResp::default()));
    assert_eq!(
        facts(&out),
        [&Fact::BdfDownloaded {
            chunks: 1,
            crc_rejected: false
        }]
    );

    // one byte more is two chunks
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![7; 6145]));
    let (txn, q) = request::<BdfDownloadReq>(&out);
    assert_eq!((q.end, q.data.as_ref().unwrap().len()), (Some(0), 6144));
    let out = r.msg(&resp(txn, &BdfDownloadResp::default()));
    let (_, q) = request::<BdfDownloadReq>(&out);
    assert_eq!(
        (q.end, q.data.as_ref().unwrap().len(), q.seg_id),
        (Some(1), 1, Some(1))
    );

    // no data at all: no chunk, the calibration report follows
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![]));
    assert_eq!(
        facts(&out),
        [&Fact::BdfDownloaded {
            chunks: 0,
            crc_rejected: false
        }]
    );
    let (_, _) = request::<CalReportReq>(&out);

    // a failure on a middle chunk is fatal
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![7; 7000]));
    let (txn, _) = request::<BdfDownloadReq>(&out);
    let out = r.msg(&resp(
        txn,
        &BdfDownloadResp {
            resp: fail(codec::ERR_MALFORMED_MSG),
        },
    ));
    assert_eq!(
        failed(&out),
        Some(Failure::Rejected {
            step: Step::BdfDownload,
            error: codec::ERR_MALFORMED_MSG
        })
    );
    assert_eq!(r.c.state(), State::Failed);

    // on the last chunk the C lets a CRC failure through; since it compares `result` with 1, any failure
    for error in [codec::ERR_MALFORMED_MSG, codec::ERR_INTERNAL, 0x1234] {
        let mut r = at_board_data();
        let out = r.go(Input::BoardData(vec![7; 100]));
        let (txn, _) = request::<BdfDownloadReq>(&out);
        let out = r.msg(&resp(txn, &BdfDownloadResp { resp: fail(error) }));
        assert_eq!(
            facts(&out),
            [&Fact::BdfDownloaded {
                chunks: 1,
                crc_rejected: true
            }],
            "error {error}"
        );
        let (_, _) = request::<CalReportReq>(&out);
    }

    // a result other than 0 or 1 on the last chunk is a real failure
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![7; 100]));
    let (txn, _) = request::<BdfDownloadReq>(&out);
    let out = r.msg(&resp(
        txn,
        &BdfDownloadResp {
            resp: Response {
                result: 2,
                error: 1,
            },
        },
    ));
    assert!(failed(&out).is_some());

    // no board file
    let mut r = at_board_data();
    let out = r.go(Input::BoardDataUnavailable);
    assert_eq!(failed(&out), Some(Failure::BoardData));
}

#[test]
fn the_calibration_report_never_fails_the_bring_up() {
    // rejected
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![1; 10]));
    let out = r.answer::<BdfDownloadReq, _>(&out, &BdfDownloadResp::default());
    let out = r.answer::<CalReportReq, _>(
        &out,
        &CalReportResp {
            resp: fail(codec::ERR_INTERNAL),
        },
    );
    assert_eq!(facts(&out), [&Fact::CalReported { accepted: false }]);
    assert_eq!(r.c.state(), State::AwaitingFwReady);

    // timed out
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![]));
    let (_, _) = request::<CalReportReq>(&out);
    let out = r.go(Input::Timeout);
    assert_eq!(facts(&out), [&Fact::CalReported { accepted: false }]);
    assert!(failed(&out).is_none());
    assert_eq!(r.c.state(), State::AwaitingFwReady);

    // undecodable
    let mut r = at_board_data();
    let out = r.go(Input::BoardData(vec![]));
    let (txn, _) = request::<CalReportReq>(&out);
    let mut bad = resp(txn, &CalReportResp::default());
    bad.truncate(HEADER_LEN + 1);
    bad[5] = 1;
    let out = r.msg(&bad);
    assert_eq!(facts(&out), [&Fact::CalReported { accepted: false }]);
}

#[test]
fn xo_calibration_is_passed_through() {
    let mut cfg = config();
    cfg.xo_cal_supported = true;
    cfg.xo_cal_data = 0xa5;
    cfg.msa_fixed_perm = true;
    let mut r = Run::new(cfg);
    let out = r.go(Input::ServerArrive);
    let (_, q) = request::<IndRegisterReq>(&out);
    assert_eq!(q.xo_cal_enable, Some(1));
    let out = r.answer::<IndRegisterReq, _>(&out, &IndRegisterResp::default());
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![]));
    let out = r.answer::<MsaReadyReq, _>(&out, &MsaReadyResp::default());
    let _ = r.answer::<CapReq, _>(&out, &cap_resp());
    let _ = r.msg(&ind(&MsaReadyInd));
    let out = r.go(Input::BoardData(vec![]));
    let (_, q) = request::<CalReportReq>(&out);
    assert_eq!(q.xo_cal_data, Some(0xa5));
}

#[test]
fn capabilities_defaults_and_memory() {
    let mut r = Run::new(config());
    let out = r.registered(None);
    let out = r.answer::<HostCapReq, _>(&out, &HostCapResp::default());
    let out = r.answer::<MsaInfoReq, _>(&out, &msa_info_resp(vec![]));
    let out = r.answer::<MsaReadyReq, _>(&out, &MsaReadyResp::default());
    // no chip or board info: 0xFF, as the C stores
    let _ = r.answer::<CapReq, _>(&out, &CapResp::default());
    let c = r.c.capabilities();
    assert_eq!(
        (c.chip_id, c.board_id, c.chip_family, c.soc_id),
        (0xff, 0xff, 0, 0)
    );
    let out = r.msg(&ind(&MsaReadyInd));
    assert_eq!(
        out.last(),
        Some(&Output::NeedBoardData {
            board_id: 0xff,
            chip_id: 0xff
        })
    );
}

#[test]
fn unexpected_messages_are_ignored() {
    let mut r = Run::new(config());
    let out = r.go(Input::ServerArrive);
    let (txn, _) = request::<IndRegisterReq>(&out);
    let good = resp(txn, &IndRegisterResp::default());

    // wrong transaction
    assert!(
        r.msg(&resp(txn + 1, &IndRegisterResp::default()))
            .is_empty()
    );
    // right transaction, another operation's response
    assert!(r.msg(&resp(txn, &HostCapResp::default())).is_empty());
    // a request or an unknown type
    assert!(
        r.msg(&encode_message(REQUEST, txn, &IndRegisterResp::default()).unwrap())
            .is_empty()
    );
    let mut odd = good.clone();
    odd[0] = 9;
    assert!(r.msg(&odd).is_empty());
    // too short, empty, unknown indication
    assert!(r.msg(&[]).is_empty());
    assert!(r.msg(&good[..5]).is_empty());
    let mut other = ind(&FwReadyInd);
    other[3] = 0x35; // REQUEST_MEM_IND, which this ath10k has no handler for
    assert!(r.msg(&other).is_empty());
    // a wrong-length indication fails to decode and is dropped
    let mut bad = ind(&FwReadyInd);
    bad[5] = 9;
    assert!(r.msg(&bad).is_empty());
    assert_eq!(r.c.state(), State::Awaiting(Step::IndRegister));

    // and the real one still works
    let out = r.msg(&good);
    assert!(failed(&out).is_none());
    assert!(r.c.state() != State::Failed);
    // the same response again has no request to answer
    assert!(r.msg(&good).is_empty());
}

#[test]
fn undecodable_and_rejected_responses_fail_the_step() {
    let mut r = Run::new(config());
    let out = r.go(Input::ServerArrive);
    let (txn, _) = request::<IndRegisterReq>(&out);
    let mut bad = resp(txn, &IndRegisterResp::default());
    bad.extend_from_slice(&[0x05, 0, 0]); // an unknown mandatory TLV
    bad[5] += 3;
    let out = r.msg(&bad);
    assert_eq!(
        failed(&out),
        Some(Failure::Decode {
            step: Step::IndRegister,
            error: Error::UnknownMandatoryTlv(5)
        })
    );
    assert_eq!(r.c.state(), State::Failed);
    // nothing moves a failed machine except a new arrival
    assert!(r.go(Input::Timeout).is_empty());
    assert!(r.msg(&ind(&MsaReadyInd)).is_empty());
    let out = r.go(Input::ServerArrive);
    let (txn, _) = request::<IndRegisterReq>(&out);
    let out = r.msg(&resp(
        txn,
        &IndRegisterResp {
            resp: fail(codec::ERR_INCOMPATIBLE_STATE),
            fw_status: None,
        },
    ));
    assert_eq!(
        failed(&out),
        Some(Failure::Rejected {
            step: Step::IndRegister,
            error: codec::ERR_INCOMPATIBLE_STATE
        })
    );
}

#[test]
fn a_timeout_names_the_request_that_was_out() {
    let mut r = Run::new(config());
    let _ = r.go(Input::ServerArrive);
    let out = r.go(Input::Timeout);
    assert_eq!(
        failed(&out),
        Some(Failure::Timeout {
            step: Step::IndRegister
        })
    );
    // a timeout with nothing out is nothing
    assert!(r.go(Input::Timeout).is_empty());
}

fn ready() -> Run {
    let mut r = Run::new(config());
    let _ = r.registered(Some(fw_status::FW_READY));
    assert_eq!(r.c.state(), State::Ready);
    r
}

#[test]
fn enable_only_when_the_firmware_is_ready_and_idle() {
    let mut r = Run::new(config());
    let out = r.go(Input::WlanEnable {
        cfg: WlanEnableCfg::default(),
        mode: 0,
    });
    assert_eq!(failed(&out), Some(Failure::NotReady));
    let out = r.go(Input::WlanDisable);
    assert_eq!(failed(&out), Some(Failure::NotReady));
    let out = r.go(Input::SetFwLogMode(1));
    assert_eq!(failed(&out), Some(Failure::NotReady));
    assert_eq!(r.c.state(), State::Idle);

    let mut r = ready();
    let out = r.go(Input::WlanEnable {
        cfg: WlanEnableCfg::default(),
        mode: 0,
    });
    let (txn, q) = request::<WlanCfgReq>(&out);
    // empty tables are still sent, as present-but-empty
    assert_eq!(q.tgt_cfg, Some(vec![]));
    assert_eq!(q.svc_cfg, Some(vec![]));
    assert_eq!(q.shadow_reg, Some(vec![]));
    // a second request while one is out
    let out2 = r.go(Input::WlanDisable);
    assert_eq!(failed(&out2), Some(Failure::Busy));
    // a rejected configuration leaves the firmware ready for another try
    let out = r.msg(&resp(
        txn,
        &WlanCfgResp {
            resp: fail(codec::ERR_INVALID_ID),
        },
    ));
    assert_eq!(
        failed(&out),
        Some(Failure::Rejected {
            step: Step::WlanCfg,
            error: codec::ERR_INVALID_ID
        })
    );
    assert_eq!(r.c.state(), State::Ready);
    let out = r.go(Input::WlanEnable {
        cfg: WlanEnableCfg::default(),
        mode: driver_mode::FTM,
    });
    let (txn, _) = request::<WlanCfgReq>(&out);
    let out = r.msg(&resp(txn, &WlanCfgResp::default()));
    let (txn, q) = request::<WlanModeReq>(&out);
    assert_eq!(q.mode, driver_mode::FTM);
    // the mode request times out
    let out = r.go(Input::Timeout);
    assert_eq!(
        failed(&out),
        Some(Failure::Timeout {
            step: Step::WlanMode
        })
    );
    assert_eq!(r.c.state(), State::Ready);
    // a late answer finds nobody waiting
    assert!(r.msg(&resp(txn, &WlanModeResp::default())).is_empty());
}

#[test]
fn transaction_ids_run_from_zero_to_65534_and_round() {
    let mut r = ready();
    // registration used 0
    let mut expect = 1u32;
    for _ in 0..70_000 {
        let out = r.go(Input::SetFwLogMode(0));
        let (txn, _) = request::<IniReq>(&out);
        assert_eq!(u32::from(txn), expect);
        assert_ne!(txn, u16::MAX);
        let _ = r.msg(&resp(txn, &IniResp::default()));
        expect = if expect == 65_534 { 0 } else { expect + 1 };
    }
}

#[test]
fn headers_of_what_it_sends() {
    let mut r = Run::new(config());
    let out = r.go(Input::ServerArrive);
    let Output::Send(b) = &out[0] else { panic!() };
    let (h, body) = Header::parse(b).unwrap();
    assert_eq!(h.msg_type, REQUEST);
    assert_eq!(h.msg_id, 0x20);
    assert_eq!(usize::from(h.msg_len), body.len());
}
