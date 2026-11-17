use alloc::{vec, vec::Vec};

use crate::{
    DataSegment, Error, Host, ResetPolicy, Started, UnitRole, WritePolicy,
    desc::{self, UnitDesc},
    model::{DATA_DADDR, Lu, Model, ModelArea, ModelClock, ModelRegs},
    policy::ReadOnly,
    regs, scsi,
    upiu::{self, DataDir, Utrd},
};

type TestHost = Host<ModelRegs, ModelArea, ModelClock>;

/// The Galaxy Book Go's layout in miniature: a big user LUN 0, two boot LUNs, small firmware LUNs, one disabled.
fn laptop_lus() -> Vec<Lu> {
    let mut boot_a = Lu::new(1, 16, 12);
    boot_a.boot_lun_id = 1;
    let mut boot_b = Lu::new(2, 16, 12);
    boot_b.boot_lun_id = 2;
    let cdt = Lu::new(3, 8, 12);
    let firmware = Lu::new(4, 64, 12);
    let mut disabled = Lu::new(5, 8, 12);
    disabled.enabled = 0;
    vec![Lu::new(0, 256, 12), boot_a, boot_b, cdt, firmware, disabled]
}

fn start(m: &Model, reset: ResetPolicy, policy: WritePolicy) -> Result<TestHost, Error> {
    Host::start(m.regs(), m.area(), m.clock(), reset, policy)
}

/// [`start`] with the error handler's reset off, what a Qualcomm host runs by default (`ufs.recover`, patch 1359).
/// The tests that use it are about the request path that is left when nothing is reset: a hung request ending the
/// host, and the buffers of a request that was not proved over being kept. [`the_reset_brings_a_hung_request_back`]
/// and its neighbours are the same failures with the reset on.
fn start_no_recover(m: &Model, reset: ResetPolicy, policy: WritePolicy) -> Result<TestHost, Error> {
    Host::start_with(
        m.regs(),
        m.area(),
        m.clock(),
        reset,
        policy,
        crate::Options {
            restart_lists: true,
            recover: false,
        },
    )
}

fn seg(off: u64, len: usize) -> DataSegment {
    DataSegment {
        daddr: DATA_DADDR + off,
        len,
    }
}

#[test]
fn cold_controller_is_reset_and_brought_up() {
    let m = Model::cold(laptop_lus());
    let h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    assert_eq!(h.started(), Started::Reset);
    let s = m.0.borrow();
    assert!(s.n.uic.contains(&regs::UIC_DME_LINK_STARTUP));
    // fDeviceInit was set, then polled until the device cleared it.
    assert!(
        s.n.queries
            .contains(&(upiu::QUERY_OP_SET_FLAG, desc::FLAG_DEVICE_INIT, 0))
    );
    assert!(!s.device_init);
    drop(s);
    let luns: Vec<u8> = h.units().iter().map(|u| u.lun).collect();
    assert_eq!(luns, [0, 1, 2, 3, 4], "the disabled LUN 5 is not a unit");
    assert_eq!(h.device().unwrap().spec_version, 0x0310);
}

#[test]
fn only_the_main_user_lun_is_writable() {
    let m = Model::cold(laptop_lus());
    let h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let role = |lun| h.unit(lun).unwrap().role;
    assert_eq!(role(0), UnitRole::Main);
    assert_eq!(role(1), UnitRole::ReadOnly(ReadOnly::NotMain));
    assert_eq!(role(4), UnitRole::ReadOnly(ReadOnly::NotMain));
    let u0 = h.unit(0).unwrap();
    assert_eq!((u0.block_size, u0.blocks), (4096, 256));
    assert_eq!(&u0.inquiry.unwrap().product, b"UFS LU          ");
}

#[test]
fn firmware_link_is_reused_without_reset() {
    let m = Model::firmware_left(laptop_lus());
    let h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    assert_eq!(h.started(), Started::Reused);
    let s = m.0.borrow();
    assert_eq!(s.n.hce_writes, 0, "HCE untouched");
    // The laptop's case: the link is active, so it is only looked at (no startup, no hibernate exit).
    assert!(
        s.n.uic
            .iter()
            .all(|c| matches!(*c, regs::UIC_DME_GET | regs::UIC_DME_PEER_GET))
    );
    assert!(
        !s.n.queries.iter().any(|q| q.0 == upiu::QUERY_OP_SET_FLAG),
        "fDeviceInit not set again"
    );
    // The lists moved from the firmware's memory to the driver's area; interrupts are off (the driver polls).
    assert_eq!(s.utrlba as u64, crate::model::AREA_DADDR);
    assert_eq!(s.ie, 0);
    // The firmware's running list is stopped, moved, then started again.
    let base = crate::model::AREA_DADDR as u32;
    assert_eq!(s.n.list_writes, [("rsr", 0), ("ba", base), ("rsr", 1)]);
    assert_eq!(
        h.probe().ie,
        0x30c5f,
        "the probe kept what the firmware left"
    );
}

#[test]
fn hibernated_link_is_woken_not_reset() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().hibernated = true;
    let h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    assert_eq!(h.started(), Started::Reused);
    let s = m.0.borrow();
    let exits =
        s.n.uic
            .iter()
            .filter(|c| **c == regs::UIC_DME_HIBER_EXIT)
            .count();
    assert_eq!(exits, 1);
    assert!(
        !s.n.uic.contains(&regs::UIC_DME_PEER_GET),
        "a hibernating link cannot answer"
    );
    assert_eq!(s.n.hce_writes, 0);
    assert!(
        s.notes.iter().any(|n| n.contains("hibernating")),
        "{:?}",
        s.notes
    );
    assert!(
        s.notes.iter().any(|n| n.contains("UHXS set, PWR_LOCAL")),
        "{:?}",
        s.notes
    );
}

#[test]
fn never_policy_leaves_a_cold_controller_alone() {
    let m = Model::cold(laptop_lus());
    let e = start(&m, ResetPolicy::Never, WritePolicy::default())
        .err()
        .unwrap();
    assert_eq!(e, Error::NotEnabled);
    assert_eq!(
        m.0.borrow().n.reg_writes,
        0,
        "not a single register written"
    );
}

#[test]
fn reuse_falls_back_to_reset_when_allowed() {
    let m = Model::firmware_left(laptop_lus());
    // The firmware's link does not answer (and DME_HIBER_EXIT does not help): a device that hangs NOP OUT.
    m.0.borrow_mut().hibernated = true;
    m.0.borrow_mut().hcs &= !regs::HCS_UCRDY;
    let h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    assert_eq!(h.started(), Started::Reset);
}

#[test]
fn read_and_write_main_lun_through_several_segments() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let pattern: Vec<u8> = (0..3 * 4096).map(|i| (i * 7 % 251) as u8).collect();
    m.mem_write(DATA_DADDR, &pattern);
    // Three blocks from one page and two half pages.
    h.write(0, 10, 3, &[seg(0, 4096), seg(4096, 4096), seg(8192, 4096)])
        .unwrap();
    assert_eq!(&m.lu(0).data[10 * 4096..13 * 4096], &pattern[..]);
    h.read(0, 9, 2, &[seg(0x10000, 2048), seg(0x20000, 6144)])
        .unwrap();
    let got = [
        m.mem_read(DATA_DADDR + 0x10000, 2048),
        m.mem_read(DATA_DADDR + 0x20000, 6144),
    ]
    .concat();
    assert_eq!(got[0], 0, "block 9 untouched: LUN byte");
    assert_eq!(got[1..9], 9u64.to_le_bytes());
    assert_eq!(&got[4096..], &pattern[..4096]);
    h.flush(0).unwrap();
}

#[test]
fn writes_to_other_luns_never_reach_the_device() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let before: Vec<Vec<u8>> = (1..=4).map(|l| m.lu(l).data).collect();
    let sent_before = m.0.borrow().n.scsi.len();
    for lun in 1..=4 {
        assert_eq!(
            h.write(lun, 0, 1, &[seg(0, 4096)]),
            Err(Error::Refused {
                lun,
                opcode: scsi::WRITE_10
            })
        );
        assert_eq!(
            h.flush(lun),
            Err(Error::Refused {
                lun,
                opcode: scsi::SYNCHRONIZE_CACHE_10
            })
        );
        // Raw commands too, including the 16-byte write and things that destroy data on any unit.
        let (cdb, n) = scsi::rw(true, 1 << 33, 1);
        assert!(
            h.scsi(lun, &cdb[..n], DataDir::ToDevice, &[seg(0, 4096)])
                .is_err()
        );
        for op in [0x04u8, 0x42, 0x41, 0x93, 0x3b, 0x55, 0xb5, 0x0a, 0x1b] {
            let r = h.scsi(lun, &[op, 0, 0, 0, 0, 0, 0, 0, 0, 0], DataDir::None, &[]);
            assert_eq!(r, Err(Error::Refused { lun, opcode: op }));
        }
    }
    // Destructive commands are refused on the main LUN as well; well-known LUNs get nothing at all.
    for op in [0x04u8, 0x42, 0x41, 0x93, 0x3b, 0x55, 0xb5] {
        assert_eq!(
            h.scsi(0, &[op, 0, 0, 0, 0, 0, 0, 0, 0, 0], DataDir::None, &[]),
            Err(Error::Refused { lun: 0, opcode: op })
        );
    }
    for wlun in [
        desc::WLUN_BOOT,
        desc::WLUN_RPMB,
        desc::WLUN_UFS_DEVICE,
        desc::WLUN_REPORT_LUNS,
    ] {
        assert!(matches!(
            h.scsi(wlun, &scsi::test_unit_ready(), DataDir::None, &[]),
            Err(Error::Refused { .. })
        ));
    }
    assert_eq!(
        m.0.borrow().n.scsi.len(),
        sent_before,
        "nothing refused was sent"
    );
    let after: Vec<Vec<u8>> = (1..=4).map(|l| m.lu(l).data).collect();
    assert_eq!(before, after);
    // Reads of the other units work.
    h.read(4, 63, 1, &[seg(0, 4096)]).unwrap();
    assert_eq!(
        m.mem_read(DATA_DADDR, 9),
        [&[4u8][..], &63u64.to_le_bytes()].concat()
    );
}

#[test]
fn read_only_policy_refuses_every_write() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::READ_ONLY).unwrap();
    assert_eq!(
        h.unit(0).unwrap().role,
        UnitRole::ReadOnly(ReadOnly::WritesDisabled)
    );
    assert!(matches!(
        h.write(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Refused { lun: 0, .. })
    ));
}

#[test]
fn main_lun_must_be_enabled_normal_unprotected_and_largest() {
    let check = |f: &dyn Fn(&mut Vec<Lu>), want: UnitRole| {
        let mut lus = laptop_lus();
        f(&mut lus);
        let m = Model::cold(lus);
        let h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
        assert_eq!(h.unit(0).map(|u| u.role), Some(want));
    };
    check(
        &|l| l[0].boot_lun_id = 1,
        UnitRole::ReadOnly(ReadOnly::BootLun),
    );
    check(
        &|l| l[0].write_protect = 1,
        UnitRole::ReadOnly(ReadOnly::WriteProtected),
    );
    check(
        &|l| l[0].memory_type = 3,
        UnitRole::ReadOnly(ReadOnly::NotNormalMemory),
    );
    check(
        &|l| l[4] = Lu::new(4, 512, 12),
        UnitRole::ReadOnly(ReadOnly::NotLargest),
    );
    // A 512-byte-block LUN 0 that is still the largest may be written.
    check(&|l| l[0] = Lu::new(0, 4096, 9), UnitRole::Main);
}

#[test]
fn policy_unit_descriptor_roles() {
    let unit = |lun, boot| UnitDesc {
        unit_index: lun,
        enabled: 1,
        boot_lun_id: boot,
        write_protect: 0,
        memory_type: 0,
        data_reliability: 0,
        log2_block_size: 12,
        block_count: 31_191_040,
    };
    let big = 31_191_040u64 * 4096;
    let p = WritePolicy::default();
    assert_eq!(p.role(0, &unit(0, 0), big), UnitRole::Main);
    assert_eq!(
        p.role(1, &unit(1, 1), big),
        UnitRole::ReadOnly(ReadOnly::NotMain)
    );
    assert_eq!(
        WritePolicy { main_lun: Some(1) }.role(1, &unit(1, 1), big),
        UnitRole::ReadOnly(ReadOnly::BootLun)
    );
    assert!(WritePolicy::allows(
        UnitRole::Main,
        0,
        &scsi::read_capacity_16(32)
    ));
    assert!(!WritePolicy::allows(
        UnitRole::Main,
        0,
        &[scsi::SERVICE_ACTION_IN_16, 0x11]
    ));
    assert!(!WritePolicy::allows(UnitRole::Main, 0, &[]));
}

#[test]
fn unit_attention_after_reset_is_absorbed() {
    let mut lus = laptop_lus();
    lus[0].unit_attention = true;
    let m = Model::cold(lus);
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    h.read(0, 0, 1, &[seg(0, 4096)]).unwrap();
}

#[test]
fn errors_from_the_device_are_reported() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    // Past the end: refused by the driver before it is sent.
    assert_eq!(h.read(0, 256, 1, &[seg(0, 4096)]), Err(Error::BadBuffer));
    // Buffer and block count disagree.
    assert_eq!(h.read(0, 0, 2, &[seg(0, 4096)]), Err(Error::BadBuffer));
    // A raw READ past the end gets CHECK CONDITION with the sense data parsed.
    let (cdb, n) = scsi::rw(false, 300, 1);
    match h.scsi(0, &cdb[..n], DataDir::FromDevice, &[seg(0, 4096)]) {
        Err(Error::Scsi {
            status: 2,
            sense: Some(s),
        }) => assert_eq!((s.key, s.asc), (5, 0x21)),
        r => panic!("{r:?}"),
    }
    assert_eq!(h.read(9, 0, 1, &[seg(0, 4096)]), Err(Error::NoUnit(9)));
    // Unaligned buffers never reach the PRDT.
    assert_eq!(h.read(0, 0, 1, &[seg(2, 4096)]), Err(Error::BadBuffer));
}

/// A SCSI command that hangs, with nothing reset after it, is taken back and ends the host: the device may still
/// hold its tag. The reset that Linux sends instead is [`the_reset_brings_a_hung_request_back`].
#[test]
fn a_hung_request_ends_the_host() {
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().hang_next = true;
    assert_eq!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Timeout("a transfer request"))
    );
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
    assert!(m.0.borrow().notes.iter().any(|n| n == "platform report"));
}

#[test]
fn a_request_that_cannot_be_cleared_kills_the_host() {
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.hang_next = true;
        s.clear_works = false;
    }
    assert!(h.read(0, 0, 1, &[seg(0, 4096)]).is_err());
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
}

/// Patch 1359, Linux's error handler: a request that will not end is answered by a reset of the host and the link
/// (`ufshcd_eh_timed_out` → `ufshcd_link_recovery`, `ufshcd_err_handler` → `ufshcd_reset_and_restore`), and the
/// command goes again. Here that is one `full_init`, the bring-up's own, so the read comes back with its data and
/// nothing was ever written outside LUN 0.
#[test]
fn the_reset_brings_a_hung_request_back() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let pattern: Vec<u8> = (0..4096).map(|i| (i * 11 % 251) as u8).collect();
    m.mem_write(DATA_DADDR, &pattern);
    h.write(0, 3, 1, &[seg(0, 4096)]).unwrap();
    m.mem_write(DATA_DADDR, &vec![0u8; 4096]);
    let hce_before = m.0.borrow().n.hce_writes;
    // The traffic of the recovery, counted from here: what the reset may put on the wire is the bring-up's own, and
    // the replayed request keeps the LUN of the one it replays ([`Host::execute`]). The boundary is judged on what the
    // controller accepted ([`Counters::submitted`]), so a command that hung before the device saw it is counted too.
    let marked = m.0.borrow();
    let submitted_before = marked.n.submitted.len();
    let queries_before = marked.n.queries.len();
    drop(marked);
    m.0.borrow_mut().hang_next = true;
    assert_eq!(h.read(0, 3, 1, &[seg(0, 4096)]), Ok(()));
    assert!(!h.is_dead());
    assert_eq!(m.mem_read(DATA_DADDR, 4096), pattern);
    let s = m.0.borrow();
    assert!(
        s.n.hce_writes >= hce_before + 2,
        "the controller was not stopped and started again: {}",
        s.n.hce_writes
    );
    assert!(
        s.n.uic.contains(&regs::UIC_DME_LINK_STARTUP),
        "the link was not started again"
    );
    assert!(
        s.n.queries
            .contains(&(upiu::QUERY_OP_SET_FLAG, desc::FLAG_DEVICE_INIT, 0)),
        "the device was not initialized again"
    );
    // The hung request's data never went anywhere, so the retry is the second READ (10) of the burst.
    let reads =
        s.n.scsi
            .iter()
            .filter(|(lun, op)| *lun == 0 && *op == scsi::READ_10)
            .count();
    assert!(reads >= 2, "the command was not sent again: {reads} reads");
    // What the recovery itself sent, and nothing outside it: no SCSI command for a unit but the main user LUN, and
    // no query but the device-initialization flag. A reset that reached another unit, or wrote a descriptor or an
    // attribute there, would show up in this suffix.
    for (lun, op) in &s.n.submitted[submitted_before..] {
        assert_eq!(*lun, 0, "the recovery touched another LUN: {op:#04x}");
    }
    for (op, idn, index) in &s.n.queries[queries_before..] {
        assert!(
            *index == 0
                && *idn == desc::FLAG_DEVICE_INIT
                && (*op == upiu::QUERY_OP_SET_FLAG || *op == upiu::QUERY_OP_READ_FLAG),
            "a query the recovery has no business sending: op {op:#x}, idn {idn:#x}, index {index}"
        );
    }
    let log = s.notes.join("\n");
    assert!(log.contains("recovery: link up"), "{log}");
    assert!(log.contains("no device reset"), "{log}");
}

/// The boundary the write rule holds for a replayed request: it is the same command, to the same LUN, with the same
/// buffers. A recovery that moved a read-only unit's request to the main LUN - or the main LUN's to a boot LUN -
/// would show up here as traffic it never sent.
#[test]
fn the_replay_goes_to_the_lun_of_the_request_it_replays() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let s0 = m.0.borrow();
    let submitted_before = s0.n.submitted.len();
    let queries_before = s0.n.queries.len();
    drop(s0);
    m.0.borrow_mut().hang_next = true;
    // LUN 4 is a firmware unit: readable, never written, and not the main LUN.
    assert_eq!(h.read(4, 2, 1, &[seg(0, 4096)]), Ok(()));
    assert!(!h.is_dead());
    let s = m.0.borrow();
    assert!(
        !s.n.submitted[submitted_before..].is_empty(),
        "the request was never sent again"
    );
    // Every command after the hang is the same read to the same unit: the replay did not move to the main LUN, and
    // the recovery itself put no SCSI command anywhere. (Two of them: the reset leaves the device answering a UNIT
    // ATTENTION once, which is sent again as every retryable status is.) The hung first attempt is in this trace
    // too, which is the point of the submission record.
    for (lun, op) in &s.n.submitted[submitted_before..] {
        assert_eq!(
            (*lun, *op),
            (4, scsi::READ_10),
            "the recovery sent other traffic, or moved the request: {:?}",
            &s.n.submitted[submitted_before..]
        );
    }
    for (op, idn, index) in &s.n.queries[queries_before..] {
        assert!(
            *index == 0
                && *idn == desc::FLAG_DEVICE_INIT
                && (*op == upiu::QUERY_OP_SET_FLAG || *op == upiu::QUERY_OP_READ_FLAG),
            "a query the recovery has no business sending: op {op:#x}, idn {idn:#x}, index {index}"
        );
    }
    let log = s.notes.join("\n");
    assert!(log.contains("recovery: link up"), "{log}");
}

/// Finding 1 of the port review, the case `ufs.recover=on` has to answer: the first attempt hangs, the reset works,
/// the replay hangs too with UTRLCLR having taken the slot back. The replay is a hung request like any other - the
/// device may hold task tag 0 again - so the host ends, its buffers stay quarantined, and no later request gets the
/// tag back. [`a_hung_request_ends_the_host`] is the same end with the reset off.
#[test]
fn a_replay_that_hangs_ends_the_host_like_the_request_it_replays() {
    use alloc::sync::Arc;
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    // Both the first attempt and the replay after the reset never complete.
    m.0.borrow_mut().hang_scsi = 2;
    let owner = Arc::new(seg(0, 4096));
    let mut quarantine = Vec::new();
    assert_eq!(
        h.read(0, 0, 1, &[*owner]),
        Err(Error::Timeout("a transfer request"))
    );
    let s = m.0.borrow();
    assert!(h.is_dead(), "a hung replay is a hung request");
    assert_eq!(s.utrldbr & 1, 0, "UTRLCLR took the slot back");
    let log = s.notes.join("\n");
    assert!(log.contains("recovery: link up"), "{log}");
    assert!(
        log.contains("a SCSI command timed out: no more requests"),
        "{log}"
    );
    drop(s);
    // The replay's own buffers are what is kept: the reset proved the first request over, the hung replay did not.
    assert_eq!(h.keep_held([owner.clone()], &mut quarantine), 4096);
    assert_eq!(Arc::strong_count(&owner), 2, "nothing frees them");
    // A later request is refused before the controller sees it: tag 0 is not handed out twice.
    let bells = m.0.borrow().bells;
    assert_eq!(h.read(0, 1, 1, &[*owner]), Err(Error::Dead));
    assert_eq!(m.0.borrow().bells, bells, "nothing reached the controller");
}

/// The resume case of patch 1353 with the answer patch 1359 gives, as the whole sequence the wake runs
/// ([`Host::resume`], what `ctl_wake` calls): the link went to hibern8 for the sleep, the way back does not bring it
/// up, the error handler is asked once, and the device answers the proof with the UNIT ATTENTION a reset always
/// asks for - which is retried, not taken for a broken link. The disk works again and the resume says so.
#[test]
fn a_wake_that_does_not_bring_the_link_back_is_reset() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_upmcrs = 3; // PWR_BUSY: the peer kept the link
    let s0 = m.0.borrow();
    let scsi_before = s0.n.scsi.len();
    let uic_before = s0.n.uic.len();
    drop(s0);
    let r = h.resume(0);
    assert!(
        r.proved(),
        "the resume did not prove its link: {:?}",
        r.failed
    );
    assert!(r.reset, "the link came back through a reset, not the exit");
    assert!(!r.hibernating);
    assert!(!h.is_dead());
    let s = m.0.borrow();
    // One reset, and the proof it took to believe it: the attention the reset caused, then the answer.
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_LINK_STARTUP)
            .count(),
        1,
        "the resume reset the link more than once"
    );
    assert_eq!(
        &s.n.scsi[scsi_before..],
        &[(0, scsi::TEST_UNIT_READY), (0, scsi::TEST_UNIT_READY)],
        "the proof was not the bounded TUR sequence of a reset device"
    );
    let log = s.notes.join("\n");
    assert!(log.contains("is not PWR_LOCAL"), "{log}");
    assert!(log.contains("resume: hibern8 exit"), "{log}");
    assert!(log.contains("recovery: link up"), "{log}");
    drop(s);
    // The disk works both ways again.
    let pattern: Vec<u8> = (0..4096).map(|i| (i * 5 % 251) as u8).collect();
    m.mem_write(DATA_DADDR, &pattern);
    h.write(0, 7, 1, &[seg(0, 4096)]).unwrap();
    m.mem_write(DATA_DADDR, &vec![0u8; 4096]);
    assert_eq!(h.read(0, 7, 1, &[seg(0, 4096)]), Ok(()));
    assert_eq!(m.mem_read(DATA_DADDR, 4096), pattern);
}

/// The same resume with the handler switched off (`ufs.recover=off`, what a Qualcomm controller runs by default,
/// patch 1359): the call declines, the controller is left exactly as the failed exit left it and the host is not
/// marked dead, so a request after the link does come back reaches the disk as it always did.
#[test]
fn a_declined_recovery_changes_nothing() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_upmcrs = 3; // PWR_BUSY: the peer kept the link
    assert!(h.hibernate_exit().is_err());
    let before = m.0.borrow().n.hce_writes;
    assert_eq!(h.error_recovery(), Err(Error::Dead));
    assert!(!h.is_dead(), "a declined reset is not a dead host");
    let s = m.0.borrow();
    assert_eq!(s.n.hce_writes, before, "the host was not reset");
    assert!(
        !s.n.uic.contains(&regs::UIC_DME_LINK_STARTUP),
        "the link was not restarted"
    );
    assert!(
        s.notes.join("\n").contains("recovery: off"),
        "{}",
        s.notes.join("\n")
    );
    drop(s);
    // The exit did bring the model's link out of hibern8 - only its UPMCRS answer was distrusted - so a plain read
    // still goes through the same request path as before the decline.
    assert_eq!(h.read(0, 7, 1, &[seg(0, 4096)]), Ok(()));
}

/// The refusal of the UIC command itself, which is what Linux's `ufshcd_uic_pwr_ctrl` hands back: the controller
/// answers FAILURE, the link stays in hibern8, and `hibernate_exit` says so. The takeover of the firmware's link is
/// the only caller that ignores that answer, and it does so for a link it could not read ([`a_failed_exit_does_not_end_bring_up`]).
#[test]
fn hibernate_exit_reports_a_refused_uic_command() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_fails = true;
    assert_eq!(
        h.hibernate_exit(),
        Err(Error::Uic {
            cmd: regs::UIC_DME_HIBER_EXIT,
            result: 1
        })
    );
    assert!(
        m.0.borrow().hibernated,
        "a refused exit leaves the link down"
    );
}

/// The same refusal on the way back from a sleep, with the handler on: the exit tried twice, the link still down, so
/// the resume asks for the one reset it is allowed and believes the link only when a command answered after it. The
/// refusal is not swallowed to get a green resume, and it is not taken for a proved link either.
#[test]
fn a_refused_hibern8_exit_on_a_wake_is_reset_and_proved() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_fails = true;
    let s0 = m.0.borrow();
    let scsi_before = s0.n.scsi.len();
    let uic_before = s0.n.uic.len();
    drop(s0);
    let r = h.resume(0);
    assert!(
        r.proved(),
        "the resume did not prove its link: {:?}",
        r.failed
    );
    assert!(r.reset, "the exit never brought the link up");
    assert!(!r.hibernating);
    assert!(!h.is_dead());
    let s = m.0.borrow();
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_HIBER_EXIT)
            .count(),
        2,
        "the exit was not tried twice"
    );
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_LINK_STARTUP)
            .count(),
        1,
        "more than one reset for one wake"
    );
    assert_eq!(
        &s.n.scsi[scsi_before..],
        &[(0, scsi::TEST_UNIT_READY), (0, scsi::TEST_UNIT_READY)],
        "the proof was not the bounded TUR of a reset device: {:?}",
        &s.n.scsi[scsi_before..]
    );
    let log = s.notes.join("\n");
    assert!(log.contains("resume: hibern8 exit"), "{log}");
    assert!(log.contains("result 0x01 FAILURE"), "{log}");
    assert!(log.contains("recovery: link up"), "{log}");
}

/// One recovery budget for a whole resume. The proof hangs, is replayed after the reset the request itself asked
/// for, and hangs again: the resume does not spend a second reset on the same failure. Linux's `needs_reset` is one
/// `ufshcd_reset_and_restore` and what is left is an error, never another attempt.
#[test]
fn a_wake_resets_once_and_never_twice() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hang_scsi = 2;
    let s0 = m.0.borrow();
    let scsi_before = s0.n.scsi.len();
    let uic_before = s0.n.uic.len();
    drop(s0);
    let r = h.resume(0);
    assert!(
        !r.proved(),
        "a proof that never answered is not a proved link"
    );
    assert!(
        r.failed.unwrap().contains("recovery"),
        "the resume did not report the refused second reset"
    );
    assert!(!r.reset, "the reset the resume asked for was refused");
    assert!(
        !r.hibernating,
        "the link the one reset brought up is not taken for hibernating"
    );
    assert!(
        h.is_dead(),
        "a hung request is a hung request, resumed or not"
    );
    let s = m.0.borrow();
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_LINK_STARTUP)
            .count(),
        1,
        "a wake that resets the link twice"
    );
    assert_eq!(s.hang_scsi, 0, "the proof and its replay are the two hangs");
    assert!(
        s.n.scsi[scsi_before..].is_empty(),
        "the device answered something over a link that never came back: {:?}",
        &s.n.scsi[scsi_before..]
    );
    let log = s.notes.join("\n");
    assert!(
        log.contains("already reset once since a request worked"),
        "{log}"
    );
    assert!(
        log.contains("a SCSI command timed out: no more requests"),
        "{log}"
    );
}

/// Finding 2 of the Codex recheck, the case the ordinary per-burst counter allowed: both exits are refused, the
/// resume's own reset works, and the proof answers the UNIT ATTENTION a reset always leaves behind - which, transport
/// completed, is what cleared `recoveries` and reopened the budget. The retried proof then times out. Inside one
/// [`Host::resume`] the budget is the call's own ([`Host::resume_resets`]) and nothing in between replenishes it, so
/// the wake still gets exactly one reset and the disk is not believed.
#[test]
fn an_answered_status_inside_a_wake_does_not_open_its_reset_budget() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_fails = true;
    let s0 = m.0.borrow();
    let scsi_before = s0.n.scsi.len();
    let submitted_before = s0.n.submitted.len() as u32;
    let uic_before = s0.n.uic.len();
    drop(s0);
    // The proof after the reset is answered (with attention), the one that follows it never comes back.
    m.0.borrow_mut().hang_scsi_after = Some(submitted_before + 1);
    let r = h.resume(0);
    assert!(
        !r.proved(),
        "a wake whose proof ended in a timeout is not a proved link: {:?}",
        r.failed
    );
    assert!(r.reset, "the resume's own reset did happen");
    assert!(!r.hibernating, "the reset raised the link");
    let s = m.0.borrow();
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_LINK_STARTUP)
            .count(),
        1,
        "the answered status in between reopened the wake's reset budget"
    );
    // Both proofs reached the controller; the device answered one of them. The submission record is what makes the
    // hung command visible at all ([`Counters::submitted`]), the completion record stays the device's own receipt.
    assert_eq!(
        &s.n.submitted[submitted_before as usize..],
        &[(0, scsi::TEST_UNIT_READY), (0, scsi::TEST_UNIT_READY)],
        "the wake's submissions: {:?}",
        &s.n.submitted[submitted_before as usize..]
    );
    assert_eq!(
        &s.n.scsi[scsi_before..],
        &[(0, scsi::TEST_UNIT_READY)],
        "the device answered more than the one proof it saw: {:?}",
        &s.n.scsi[scsi_before..]
    );
    let log = s.notes.join("\n");
    assert!(
        log.contains("already reset once in this resume"),
        "the refusal did not name the wake's budget: {log}"
    );
    drop(s);
    assert!(
        h.is_dead(),
        "a hung proof is a hung request, resumed or not"
    );
    // The task tag of the proof that never came back is not handed out again.
    let bells = m.0.borrow().bells;
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
    assert_eq!(m.0.borrow().bells, bells, "nothing reached the controller");
}

/// The same budget with the wake's one reset spent by the proof's own request rather than by the resume: the first
/// TUR hangs, so `execute` asks for the reset and the replay answers the attention it left, and the retry that
/// follows hangs too. Neither the answered status nor the resume's own later ask gets a second reset, and nothing is
/// submitted on the tag the hung proof may still hold.
#[test]
fn a_wake_whose_reset_a_hung_proof_spent_does_not_reset_again() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    // The exits work; the first proof never answers, and that hang is what brings the link back.
    m.0.borrow_mut().hang_scsi = 1;
    let s0 = m.0.borrow();
    let scsi_before = s0.n.scsi.len();
    let submitted_before = s0.n.submitted.len() as u32;
    let uic_before = s0.n.uic.len();
    drop(s0);
    // The replay answers (attention), the retry after it hangs: submissions 2 and 3 of the wake.
    m.0.borrow_mut().hang_scsi_after = Some(submitted_before + 2);
    let r = h.resume(0);
    assert!(
        !r.proved(),
        "a wake that ends with a proof that never answered is not proved: {:?}",
        r.failed
    );
    assert!(
        !r.reset,
        "the resume's own ask was refused, so it reports the reset the proof spent, not one it made"
    );
    let s = m.0.borrow();
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_LINK_STARTUP)
            .count(),
        1,
        "the wake reset the link twice"
    );
    assert_eq!(
        s.n.submitted[submitted_before as usize..].len(),
        3,
        "the wake's submissions: {:?}",
        &s.n.submitted[submitted_before as usize..]
    );
    assert!(
        s.n.submitted[submitted_before as usize..]
            .iter()
            .all(|&(lun, op)| lun == 0 && op == scsi::TEST_UNIT_READY),
        "the wake sent something but the proof"
    );
    assert_eq!(
        &s.n.scsi[scsi_before..],
        &[(0, scsi::TEST_UNIT_READY)],
        "only the replay answered, and it answered the attention the reset left"
    );
    let log = s.notes.join("\n");
    assert!(log.contains("recovery: link up"), "{log}");
    assert!(
        log.contains("already reset once in this resume"),
        "the refusal did not name the wake's budget: {log}"
    );
    drop(s);
    assert!(
        h.is_dead(),
        "a hung proof is a hung request, resumed or not"
    );
    let bells = m.0.borrow().bells;
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
    assert_eq!(m.0.borrow().bells, bells, "nothing reached the controller");
}

/// A wake that comes back: the ordinary per-burst rule is left alone once the scope closes, so the read that fails
/// after a proved resume still gets its reset, and the request completes.
#[test]
fn a_proved_wake_leaves_later_io_its_own_reset_budget() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_upmcrs = 3; // PWR_BUSY: the peer kept the link
    let r = h.resume(0);
    assert!(
        r.proved(),
        "the wake did not prove its link: {:?}",
        r.failed
    );
    let s0 = m.0.borrow();
    let uic_before = s0.n.uic.len();
    drop(s0);
    // A failure burst of ordinary I/O, after the wake spent its one reset.
    m.0.borrow_mut().hang_next = true;
    assert_eq!(h.read(0, 3, 1, &[seg(0, 4096)]), Ok(()));
    let s = m.0.borrow();
    assert_eq!(
        s.n.uic[uic_before..]
            .iter()
            .filter(|c| **c == regs::UIC_DME_LINK_STARTUP)
            .count(),
        1,
        "ordinary I/O after a wake is not held to the wake's budget"
    );
    assert!(!h.is_dead());
}

/// A refused exit with the handler off (`ufs.recover=off`, what a Qualcomm controller runs): the resume says the link
/// is not proved, leaves it hibernating for the next wake, sends nothing to prove a link that never came up, and does
/// not end the host. What the requests get is the disk's own answer, as before patch 1359.
#[test]
fn a_refused_exit_with_recovery_off_leaves_the_link_unproved() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    h.hibernate_enter().unwrap();
    m.0.borrow_mut().hiber_exit_fails = true;
    let s0 = m.0.borrow();
    let scsi_before = s0.n.scsi.len();
    let hce_before = s0.n.hce_writes;
    drop(s0);
    let r = h.resume(0);
    assert!(!r.proved());
    assert!(!r.reset);
    assert!(r.hibernating, "the next wake must try the link again");
    assert!(!h.is_dead(), "a refused resume is not a dead host");
    let s = m.0.borrow();
    assert!(s.hibernated, "the model's link is where the exit left it");
    assert_eq!(s.n.hce_writes, hce_before, "nothing was reset");
    assert!(
        s.n.scsi[scsi_before..].is_empty(),
        "a command was sent over a link that never came up: {:?}",
        &s.n.scsi[scsi_before..]
    );
    let log = s.notes.join("\n");
    assert!(log.contains("resume: hibern8 exit"), "{log}");
    assert!(log.contains("recovery: off"), "{log}");
}

/// Once per failure, not five times as Linux loops (`MAX_HOST_RESET_RETRIES`, `ufshcd.c:81`): a request that ends
/// afterwards opens a new failure, and a reset that did not bring the controller back is not tried again - the host
/// is left dead, which is Linux's `UFSHCD_STATE_ERROR` (`:7763-7768`) and an I/O error rather than a wait.
#[test]
fn the_reset_is_tried_once_per_failure() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let base = m.0.borrow().n.hce_writes;
    // Two hung requests, each with its own reset, because the first one ended with the read completing.
    for lba in [3u64, 4] {
        m.0.borrow_mut().hang_next = true;
        assert_eq!(h.read(0, lba, 1, &[seg(0, 4096)]), Ok(()));
    }
    let after_two = m.0.borrow().n.hce_writes;
    assert!(
        after_two >= base + 4,
        "two failures, two resets: {base} -> {after_two}"
    );
    // A third failure whose reset cannot bring the device back: the request's own error, the host dead, and no
    // fourth reset behind it.
    {
        let mut s = m.0.borrow_mut();
        s.hang_next = true;
        s.hang_nop = 40;
    }
    assert_eq!(
        h.read(0, 5, 1, &[seg(0, 4096)]),
        Err(Error::Timeout("a transfer request"))
    );
    assert!(h.is_dead());
    let after_fail = m.0.borrow().n.hce_writes;
    assert_eq!(h.read(0, 6, 1, &[seg(0, 4096)]), Err(Error::Dead));
    assert_eq!(
        m.0.borrow().n.hce_writes,
        after_fail,
        "a host the reset gave up on is not reset again"
    );
    let log = m.0.borrow().notes.join("\n");
    assert!(log.contains("recovery: reset failed"), "{log}");
}

/// A recovery of a controller that will not come back is an error, not a wait and not a second try: the bring-up
/// inside it fails, the host is left dead, and the request's own error is what the caller sees.
///
/// The lifetime it leaves is the half of the recovery-on coverage that [`a_hung_request_ends_the_host`] only shows
/// with the handler off: nothing is quarantined here, because the recovery stopped the controller before anything
/// could fail - `full_init` begins by driving HCE 1→0 and waiting for it (`enable_controller`) - which is the same
/// proof [`Host::error_recovery`] leans on when it succeeds and clears `held`. Whether the bring-up finished after
/// that says nothing about memory the stopped controller can no longer touch. What a failed recovery does settle is
/// the other way round: the host is dead from here, so no later request hands it buffers at all. The request whose
/// memory *is* kept is the hung replay of
/// [`a_replay_that_hangs_ends_the_host_like_the_request_it_replays`], the case where no reset followed the command
/// that hung.
#[test]
fn a_reset_that_fails_reports_the_request_error() {
    use alloc::sync::Arc;
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.hang_next = true;
        // Every NOP OUT after the reset hangs too, so `verify_device` inside `full_init` gives up.
        s.hang_nop = 40;
    }
    let owner = Arc::new(seg(0, 4096));
    let mut quarantine = Vec::new();
    assert_eq!(
        h.read(0, 0, 1, &[*owner]),
        Err(Error::Timeout("a transfer request"))
    );
    assert!(h.is_dead());
    let log = m.0.borrow().notes.join("\n");
    assert!(log.contains("recovery: reset failed"), "{log}");
    assert_eq!(h.keep_held([owner.clone()], &mut quarantine), 0);
    assert!(
        quarantine.is_empty(),
        "the controller was stopped, not left running"
    );
    assert_eq!(Arc::strong_count(&owner), 1, "the caller frees them");
    // Nothing waits any more: the host is dead, so the next request is refused where it stands and gives the
    // stopped controller no memory either.
    let bells = m.0.borrow().bells;
    assert_eq!(h.read(0, 1, 1, &[*owner]), Err(Error::Dead));
    assert_eq!(m.0.borrow().bells, bells, "nothing reached the controller");
}

#[test]
fn no_64_bit_addressing_means_no_buffers_above_4_gib() {
    let m = Model::cold(laptop_lus());
    m.0.borrow_mut().cap &= !regs::CAP_64AS;
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let high = DataSegment {
        daddr: 0x1_0000_0000,
        len: 4096,
    };
    assert_eq!(h.read(0, 0, 1, &[high]), Err(Error::BadBuffer));
}

#[test]
fn dme_get_reads_link_attributes() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::default()).unwrap();
    let link = h.link_state();
    assert_eq!(
        (link.rx_gear, link.tx_gear, link.pwrmode),
        (Some(3), Some(3), Some(0x11))
    );
    assert!(!link.hibernating());
    // The last one read: RX FSM of lane 0, with the RX selector.
    assert_eq!(
        m.0.borrow().arg[0],
        (regs::MPHY_RX_FSM_STATE as u32) << 16 | 4
    );
}

#[test]
fn thirty_two_lu_devices_are_scanned_to_31() {
    let mut lus = laptop_lus();
    lus.push(Lu::new(20, 8, 12));
    let m = Model::cold(lus);
    m.0.borrow_mut().max_lu_32 = true;
    let h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    assert!(h.unit(20).is_some());
}

// ---- encodings, against Linux's layouts ----

#[test]
fn utrd_layout() {
    let d = Utrd {
        dir: DataDir::FromDevice,
        interrupt: true,
        ucd: 0x1_2345_6780,
        rsp_offset: 512,
        rsp_len: 512,
        prdt_offset: 1024,
        prdt_entries: 3,
    }
    .encode();
    // DW0: CT=1 (bit 28), DD=2 (bits 26:25), I (bit 24).
    assert_eq!(
        u32::from_le_bytes(d[0..4].try_into().unwrap()),
        0x1000_0000 | 2 << 25 | 1 << 24
    );
    assert_eq!(d[8], upiu::OCS_INVALID_COMMAND_STATUS);
    assert_eq!(
        u64::from_le_bytes(d[16..24].try_into().unwrap()),
        0x1_2345_6780
    );
    // Lengths and offsets in dwords: response length then offset, PRDT length (entries) then offset.
    assert_eq!(&d[24..32], &[128, 0, 128, 0, 3, 0, 0, 1]);
}

#[test]
fn prdt_entry_layout_and_limits() {
    let e = upiu::prdt_entry(0xdead_b000, 4096).unwrap();
    assert_eq!(&e[0..8], &0xdead_b000u64.to_le_bytes());
    assert_eq!(&e[12..16], &4095u32.to_le_bytes());
    assert!(upiu::prdt_entry(0x1000, 256 * 1024).is_ok());
    assert!(upiu::prdt_entry(0x1000, 256 * 1024 + 4).is_err());
    assert!(upiu::prdt_entry(0x1002, 4096).is_err());
    assert!(upiu::prdt_entry(0x1000, 4094).is_err());
    assert!(upiu::prdt_entry(0x1000, 0).is_err());
}

#[test]
fn upiu_layouts() {
    let c = upiu::command(
        3,
        7,
        DataDir::ToDevice,
        8192,
        &scsi::rw(true, 0x1234, 2).0[..10],
    );
    assert_eq!(&c[0..4], &[upiu::TT_COMMAND, upiu::CMD_FLAG_WRITE, 3, 7]);
    assert_eq!(&c[12..16], &8192u32.to_be_bytes());
    assert_eq!(&c[16..26], &[0x2a, 0, 0, 0, 0x12, 0x34, 0, 0, 2, 0]);
    let q = upiu::Query::read_desc(desc::IDN_UNIT, 4).encode(1);
    assert_eq!(
        &q[0..6],
        &[upiu::TT_QUERY_REQ, 0, 0, 1, 0, upiu::QUERY_FN_READ]
    );
    assert_eq!(
        &q[12..20],
        &[upiu::QUERY_OP_READ_DESC, desc::IDN_UNIT, 4, 0, 0, 0, 0, 255]
    );
    let s = upiu::Query::set_flag(desc::FLAG_DEVICE_INIT).encode(0);
    assert_eq!(
        (s[5], s[12], s[13]),
        (upiu::QUERY_FN_WRITE, upiu::QUERY_OP_SET_FLAG, 1)
    );
}

#[test]
fn cdbs_pick_10_or_16_bytes() {
    assert_eq!(scsi::rw(false, 31_191_039, 8).1, 10);
    let (c, n) = scsi::rw(false, 1 << 32, 1);
    assert_eq!((n, c[0]), (16, scsi::READ_16));
    assert_eq!(&c[2..10], &(1u64 << 32).to_be_bytes());
    let (c, n) = scsi::rw(true, 0, 0x1_0000);
    assert_eq!((n, c[0]), (16, scsi::WRITE_16));
    assert_eq!(&c[10..14], &0x1_0000u32.to_be_bytes());
}

#[test]
fn parsers() {
    assert_eq!(
        scsi::Sense::parse(&[0x72, 0x06, 0x29, 0x00]).unwrap().key,
        6
    );
    assert_eq!(scsi::Sense::parse(&[0x70, 0, 0x05]).unwrap().asc, 0);
    assert!(scsi::Sense::parse(&[0x00]).is_none());
    let c = scsi::Capacity::parse_16(&[0, 0, 0, 0, 0x01, 0xdb, 0xef, 0xff, 0, 0, 0x10, 0]).unwrap();
    assert_eq!((c.blocks, c.block_size), (31_191_040, 4096));
    assert!(scsi::Capacity::parse_16(&[0xff; 12]).is_err());
    // Truncated or mislabeled descriptors are rejected.
    assert!(UnitDesc::parse(&[0x2d, 0x02, 0]).is_err());
    assert!(UnitDesc::parse(&[0x2d; 0x2d]).is_err());
    assert!(desc::DeviceDesc::parse(&[0x59, 0x01]).is_err());
}

/// A GPT as sfdisk writes it for the laptop's main LUN: 4096-byte LBAs, the ESP at LBA 256-66815.
fn laptop_gpt(lu: &mut Lu) {
    let bs = 4096;
    let mut entries = vec![0u8; 128 * 128];
    let esp: [u8; 16] = [
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ];
    entries[0..16].copy_from_slice(&esp);
    entries[16] = 1;
    entries[32..40].copy_from_slice(&256u64.to_le_bytes());
    entries[40..48].copy_from_slice(&66815u64.to_le_bytes());
    for (i, c) in "efi".encode_utf16().enumerate() {
        entries[56 + 2 * i..58 + 2 * i].copy_from_slice(&c.to_le_bytes());
    }
    let mut h = vec![0u8; 92];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&1u64.to_le_bytes());
    h[72..80].copy_from_slice(&2u64.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h[88..92].copy_from_slice(&crate::gpt::crc32(&entries).to_le_bytes());
    let crc = crate::gpt::crc32(&h);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    lu.data[bs..bs + 92].copy_from_slice(&h);
    lu.data[2 * bs..2 * bs + entries.len()].copy_from_slice(&entries);
}

#[test]
fn gpt_of_the_main_lun_is_read_for_the_log() {
    let mut lus = laptop_lus();
    laptop_gpt(&mut lus[0]);
    let m = Model::cold(lus);
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::READ_ONLY).unwrap();
    let (header, entries) = h.read_gpt(0).unwrap().unwrap();
    assert!(header.crc_ok);
    assert_eq!(
        header.entries_crc,
        crate::gpt::crc32(&m.lu(0).data[2 * 4096..2 * 4096 + 128 * 128])
    );
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(
        (e.index, e.first_lba, e.last_lba, e.name.as_str()),
        (1, 256, 66815, "efi")
    );
    assert_eq!(
        crate::gpt::guid(&e.type_guid),
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
    );
    assert_eq!(crate::gpt::type_name(&e.type_guid), "EFI system");
    // A unit without a GPT, and a corrupted header CRC.
    assert_eq!(h.read_gpt(2).unwrap(), None);
    let mut lus = laptop_lus();
    laptop_gpt(&mut lus[0]);
    lus[0].data[4096 + 40] ^= 1;
    let m = Model::cold(lus);
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::READ_ONLY).unwrap();
    assert!(!h.read_gpt(0).unwrap().unwrap().0.crc_ok);
}

#[test]
fn crc32_check_value() {
    assert_eq!(crate::gpt::crc32(b"123456789"), 0xcbf4_3926);
}

/// What the laptop's first run showed: the link is active, NOP OUT fails. No hibernate exit, and the error reported
/// is NOP OUT's, not that of an exit that had nothing to do.
#[test]
fn a_failed_ping_on_an_active_link_reports_the_ping() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().nop_fails = true;
    let e = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY)
        .err()
        .unwrap();
    assert_eq!(e, Error::Ocs(upiu::OCS_PEER_COMM_FAILURE));
    let s = m.0.borrow();
    assert!(!s.n.uic.contains(&regs::UIC_DME_HIBER_EXIT));
    assert_eq!(s.n.hce_writes, 0);
    let log = s.notes.join("\n");
    assert!(log.contains("not hibernating"), "{log}");
    assert!(log.contains("device answers over the link"), "{log}");
    assert!(
        log.contains("request failed: overall command status 0x05"),
        "{log}"
    );
    assert!(
        log.contains("after failure: UTRLBA 0x80000000 RSR 1/1"),
        "{log}"
    );
    // Ten failed pings, three described.
    assert_eq!(
        s.notes
            .iter()
            .filter(|n| n.starts_with("request failed"))
            .count(),
        3
    );
}

/// State unknown (DME_GET fails) and the ping fails: try the exit; that it fails too must not hide the ping's error.
#[test]
fn a_failed_exit_does_not_end_bring_up() {
    let m = Model::firmware_left(laptop_lus());
    {
        let mut s = m.0.borrow_mut();
        s.dme_get_result = Some(1);
        s.nop_fails = true;
    }
    let e = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY)
        .err()
        .unwrap();
    assert_eq!(e, Error::Ocs(upiu::OCS_PEER_COMM_FAILURE));
    let log = m.0.borrow().notes.join("\n");
    assert!(log.contains("state unknown"), "{log}");
    assert!(
        log.contains("first failure: DME_GET (0x01): result 0x01 INVALID_ATTR"),
        "{log}"
    );
    assert!(
        log.contains("hibernate exit: DME_HIBER_EXIT (0x18): result 0x01 FAILURE"),
        "{log}"
    );
}

/// State unknown and the link really is hibernating: the exit after the failed ping brings it back.
#[test]
fn an_unknown_state_can_still_recover_from_hibernate() {
    let m = Model::firmware_left(laptop_lus());
    {
        let mut s = m.0.borrow_mut();
        s.dme_get_result = Some(1);
        s.hibernated = true;
    }
    let h = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).unwrap();
    assert_eq!(h.started(), Started::Reused);
}

#[test]
fn log_lines_fit_a_100_column_screen() {
    for setup in [
        (|_: &mut crate::model::State| {}) as fn(&mut crate::model::State),
        |s| s.nop_fails = true,
        |s| s.hibernated = true,
        |s| {
            s.dme_get_result = Some(1);
            s.nop_fails = true;
        },
    ] {
        let m = Model::firmware_left(laptop_lus());
        setup(&mut m.0.borrow_mut());
        let _ = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY);
        for n in &m.0.borrow().notes {
            // "[  123.456] NOTICE: ufs: ufs0: " takes 31 columns.
            assert!(n.len() <= 69, "{} columns: {n}", n.len());
        }
    }
}

#[test]
fn uic_results_are_decoded() {
    let e = Error::Uic {
        cmd: regs::UIC_DME_HIBER_EXIT,
        result: 1,
    };
    assert_eq!(
        alloc::format!("{e}"),
        "DME_HIBER_EXIT (0x18): result 0x01 FAILURE"
    );
    let e = Error::Uic {
        cmd: regs::UIC_DME_GET,
        result: 1,
    };
    assert_eq!(
        alloc::format!("{e}"),
        "DME_GET (0x01): result 0x01 INVALID_ATTR"
    );
    let e = Error::Uic {
        cmd: regs::UIC_DME_PEER_GET,
        result: 8,
    };
    assert_eq!(
        alloc::format!("{e}"),
        "DME_PEER_GET (0x03): result 0x08 PEER_COMM_FAILURE"
    );
    assert_eq!(regs::upmcrs_name(0x10f), "PWR_LOCAL");
    assert_eq!(alloc::format!("{}", regs::IsNames(0x400)), "UCCS");
    assert_eq!(
        alloc::format!("{}", regs::IsNames(0x10401)),
        "UTRCS UCCS HCFES"
    );
}

/// The laptop's reading: the door bell reads 0 right after it is rung. That is not completion: the driver waits for the
/// descriptor to change.
#[test]
fn a_bell_taken_late_is_not_taken_for_completion() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().bell_lag = 40;
    let h = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).unwrap();
    assert_eq!(h.started(), Started::Reused);
    assert_eq!(h.units().len(), 5);
    let s = m.0.borrow();
    assert!(
        s.notes
            .iter()
            .any(|n| n.starts_with("first request: bell read back 0x0")),
        "{:?}",
        s.notes
    );
}

/// The slot comes back and the descriptor never changes: the controller may be running another list, so nothing more
/// goes to it, and its memory stays allocated.
#[test]
fn a_descriptor_that_never_changes_stops_everything() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().base_ignored = true;
    let e = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY)
        .err()
        .unwrap();
    assert_eq!(e, Error::Ocs(upiu::OCS_INVALID_COMMAND_STATUS));
    let s = m.0.borrow();
    assert_eq!(s.bells, 1, "one door bell, then nothing");
    assert!(
        s.notes.iter().any(|n| n.contains("list base not taken")),
        "{:?}",
        s.notes
    );
    assert!(s.notes.iter().any(|n| n == "platform report"));
    assert_eq!((s.utrlrsr, s.utmrlrsr), (0, 0), "the lists are stopped");
    assert!(!s.area_freed, "the controller still points at this memory");
}

#[test]
fn a_failed_bring_up_stops_the_lists_and_keeps_the_memory() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().nop_fails = true;
    assert!(start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).is_err());
    let s = m.0.borrow();
    assert_eq!((s.utrlrsr, s.utmrlrsr), (0, 0));
    assert!(!s.area_freed);
    drop(s);
    // Before the lists were touched there is nothing to protect: a cold controller is left alone.
    let m = Model::cold(laptop_lus());
    assert!(start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).is_err());
    assert!(m.0.borrow().area_freed);
}

#[test]
fn the_lists_can_be_left_running() {
    let m = Model::firmware_left(laptop_lus());
    let options = crate::Options {
        restart_lists: false,
        recover: true,
    };
    let h = Host::start_with(
        m.regs(),
        m.area(),
        m.clock(),
        ResetPolicy::Never,
        WritePolicy::READ_ONLY,
        options,
    );
    assert!(h.is_ok());
    let s = m.0.borrow();
    assert_eq!(
        s.n.list_writes,
        [("ba", crate::model::AREA_DADDR as u32), ("rsr", 1)]
    );
}

/// What reload leaves: the lists stopped, the link up, and the next kernel can take over.
#[test]
fn quiesce_leaves_a_state_the_next_kernel_takes_over() {
    let m = Model::firmware_left(laptop_lus());
    let mut h = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).unwrap();
    h.quiesce();
    assert_eq!(h.read(0, 0, 1, &[seg(0, 4096)]), Err(Error::Dead));
    assert_eq!(m.0.borrow().utrlrsr, 0);
    drop(h);
    let h = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).unwrap();
    assert_eq!(h.started(), Started::Reused);
}

#[test]
fn leftover_requests_are_cleared_at_takeover() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().utrldbr = 0x4;
    let h = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).unwrap();
    assert_eq!(h.started(), Started::Reused);
    assert!(
        m.0.borrow()
            .notes
            .iter()
            .any(|n| n.contains("DBR 0x4 still set"))
    );
}

#[test]
fn reads_are_repeated_after_unit_attention_and_busy() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().lus[0].unit_attention = true;
    h.read(0, 0, 1, &[seg(0, 4096)]).unwrap();
}

#[test]
fn data_reliability_is_read() {
    let mut d = [0u8; 0x2d];
    d[0] = 0x2d;
    d[1] = desc::IDN_UNIT;
    d[9] = 1;
    assert_eq!(UnitDesc::parse(&d).unwrap().data_reliability, 1);
}

// ---- Linux's error and retry behaviour ----

/// D-B: IS_FATAL is Linux's INT_FATAL_ERRORS, ULLS and CEFES included.
#[test]
fn is_fatal_is_linuxs_int_fatal_errors() {
    assert_eq!(
        regs::IS_FATAL,
        regs::IS_DFES
            | regs::IS_UTPES
            | regs::IS_HCFES
            | regs::IS_SBFES
            | regs::IS_CEFES
            | regs::IS_ULLS
    );
}

/// D-B: a fatal interrupt bit fails the request and, with the reset off, kills the host - Linux's "reset failed"
/// outcome. With the reset on the same bits go through it: [`the_reset_brings_a_hung_request_back`].
#[test]
fn a_fatal_interrupt_bit_kills_the_host() {
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().hang_and_fatal = Some(regs::IS_ULLS);
    assert_eq!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Fatal(regs::IS_ULLS))
    );
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
    let log = m.0.borrow().notes.join("\n");
    assert!(log.contains("no more requests"), "{log}");
}

/// D-C: five retries (SD_MAX_RETRIES), then the request fails.
#[test]
fn five_retries_then_failure() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().ocs_fail = (10, upiu::OCS_ABORTED);
    assert_eq!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Ocs(upiu::OCS_ABORTED))
    );
    let reads =
        m.0.borrow()
            .n
            .scsi
            .iter()
            .filter(|c| **c == (0, scsi::READ_10))
            .count();
    assert_eq!(reads, 6, "one try plus SD_MAX_RETRIES retries");
}

/// D-C: SCSI TASK ABORTED (0x40) is retried like BUSY.
#[test]
fn task_aborted_is_retried() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().task_aborted = 2;
    h.read(0, 0, 1, &[seg(0, 4096)]).unwrap();
    let reads =
        m.0.borrow()
            .n
            .scsi
            .iter()
            .filter(|c| **c == (0, scsi::READ_10))
            .count();
    assert_eq!(reads, 3);
}

/// D-C: SYNCHRONIZE CACHE gets the same retries (sd_setup_flush_cmnd).
#[test]
fn flush_is_retried() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().task_aborted = 2;
    h.flush(0).unwrap();
    let syncs =
        m.0.borrow()
            .n
            .scsi
            .iter()
            .filter(|c| **c == (0, scsi::SYNCHRONIZE_CACHE_10))
            .count();
    assert_eq!(syncs, 3);
}

/// D-D: a non-zero residual with the overflow flag clear is a short transfer and fails; with the flag set it does
/// not.
#[test]
fn a_short_transfer_fails_but_an_overflowing_residual_does_not() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().residual = Some((4096, false));
    assert_eq!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::BadData("residual transfer count"))
    );
    m.0.borrow_mut().residual = Some((4096, true));
    h.read(0, 0, 1, &[seg(0, 4096)]).unwrap();
}

/// D-G: IS.UE in the poll path is read, logged and cleared; the PA_INIT class is fatal, the others are not. The
/// reset is off here, as in the tests above: which class counts as fatal is what this is about, not what follows.
#[test]
fn uic_errors_in_the_poll_path_are_logged_and_only_pa_init_is_fatal() {
    // PA_INIT in UECDL: the class Linux treats as needing a reset.
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().hang_and_ue = Some([0, regs::UECDL_PA_INIT_ERROR, 0, 0, 0]);
    assert!(matches!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Fatal(_))
    ));
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
    let log = m.0.borrow().notes.join("\n");
    assert!(
        log.contains("UIC error: PA 0x0 DL 0x2000 N 0x0 T 0x0 DME 0x0"),
        "{log}"
    );

    // A network-layer error: logged and cleared, not fatal by itself.
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().hang_and_ue = Some([0, 0, 0x8000_0000, 0, 0]);
    assert!(matches!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Timeout(_))
    ));
    let s = m.0.borrow();
    let log = s.notes.join("\n");
    assert!(
        log.contains("UIC error: PA 0x0 DL 0x0 N 0x80000000 T 0x0 DME 0x0"),
        "{log}"
    );
    assert_eq!(s.is & regs::IS_UE, 0, "IS.UE cleared");
    assert_eq!(s.uec, [0; 5], "the UEC registers cleared on read");
}

/// D-K: NOP OUT stops at the first timeout (Linux breaks on -ETIMEDOUT).
#[test]
fn nop_out_stops_at_the_first_timeout() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().hang_nop = 5;
    let e = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY)
        .err()
        .unwrap();
    assert!(matches!(e, Error::Timeout(_)), "{e:?}");
    assert_eq!(m.0.borrow().bells, 1, "one door bell, then nothing");
}

/// D-L: query requests are tried QUERY_REQ_RETRIES = 3 times.
#[test]
fn queries_are_retried_three_times() {
    let m = Model::cold(laptop_lus());
    m.0.borrow_mut().query_fail = 2;
    let h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let device_descs =
        m.0.borrow()
            .n
            .queries
            .iter()
            .filter(|q| **q == (upiu::QUERY_OP_READ_DESC, desc::IDN_DEVICE, 0))
            .count();
    assert_eq!(device_descs, 3);
    assert!(h.unit(0).is_some());
}

/// D-P: OCS is read with MASK_OCS 0x0F, so upper bits of the byte are ignored.
#[test]
fn ocs_is_masked_to_0x0f() {
    let mut u = [0u8; upiu::UTRD_SIZE];
    u[8] = 0xf6;
    assert_eq!(upiu::ocs(&u), upiu::OCS_ABORTED);
    // And a controller that sets upper bits still gets the retry.
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().ocs_fail = (10, 0xf6);
    assert_eq!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Ocs(upiu::OCS_ABORTED))
    );
}

/// D-N: after DME_HIBER_EXIT, HCS.UPMCRS must be PWR_LOCAL or the bring-up fails.
#[test]
fn hibernate_exit_without_pwr_local_fails_the_bring_up() {
    let m = Model::firmware_left(laptop_lus());
    {
        let mut s = m.0.borrow_mut();
        s.hibernated = true;
        s.hiber_exit_upmcrs = 3; // PWR_BUSY
    }
    let e = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY)
        .err()
        .unwrap();
    assert_eq!(e, Error::NoDevice);
    let log = m.0.borrow().notes.join("\n");
    assert!(log.contains("is not PWR_LOCAL"), "{log}");
}

/// D-R: UTRIACR is read once at probe, printed next to AHIT, and never written.
#[test]
fn utrilacr_is_read_and_never_written() {
    let m = Model::firmware_left(laptop_lus());
    m.0.borrow_mut().utrilacr = 0x1f05;
    let h = start(&m, ResetPolicy::Never, WritePolicy::READ_ONLY).unwrap();
    assert_eq!(h.probe().utrilacr, 0x1f05);
    let s = m.0.borrow();
    assert_eq!(s.n.utrilacr_writes, 0, "UTRIACR never written");
    assert!(
        s.notes
            .iter()
            .any(|n| n == "probe: AHIT 0x0 UTRIACR 0x1f05"),
        "{:?}",
        s.notes
    );
}

/// The owner's rule, checked against everything the model saw: no write ever reached a LUN but 0.
#[test]
fn no_write_ever_reaches_a_lun_but_0() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    h.write(0, 5, 2, &[seg(0, 8192)]).unwrap();
    h.flush(0).unwrap();
    h.read(4, 0, 1, &[seg(0, 4096)]).unwrap();
    let writes: Vec<u8> =
        m.0.borrow()
            .n
            .scsi
            .iter()
            .filter(|(_, op)| *op == scsi::WRITE_10 || *op == scsi::WRITE_16)
            .map(|(lun, _)| *lun)
            .collect();
    assert_eq!(writes, [0], "every WRITE went to LUN 0");
}

/// Unsafe audit round 2, U-1: the request that ends the host keeps its buffers when its slot never clears. With a
/// reset after it the controller is stopped and the buffers are safe, which is the other half of
/// [`the_reset_brings_a_hung_request_back`]; here nothing is reset, which is the case that needs the quarantine.
#[test]
fn a_request_whose_slot_never_clears_keeps_its_buffers() {
    use alloc::sync::Arc;
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.hang_next = true;
        s.clear_works = false;
    }
    // The block layer's bio segments: shared owners of the memory at these device addresses.
    let owners = [Arc::new(seg(0, 4096)), Arc::new(seg(0x1000, 4096))];
    let segments: Vec<DataSegment> = owners.iter().map(|o| **o).collect();
    let mut quarantine = Vec::new();
    assert!(h.read(0, 0, 2, &segments).is_err());
    assert!(h.is_dead());
    assert_eq!(h.keep_held(owners.iter().cloned(), &mut quarantine), 8192);
    let weak: Vec<_> = owners.iter().map(Arc::downgrade).collect();
    drop(owners);
    assert!(
        weak.iter().all(|w| w.strong_count() == 1),
        "the controller may still use them: nothing frees them"
    );
    assert_eq!(m.0.borrow().utrldbr & 1, 1, "slot 0 is still set");
    // Once only: a later request is refused before it reaches the controller and keeps nothing.
    let later = [Arc::new(seg(0x2000, 4096))];
    assert_eq!(h.read(0, 2, 1, &[*later[0]]), Err(Error::Dead));
    assert_eq!(h.keep_held(later.iter().cloned(), &mut quarantine), 0);
    assert_eq!(quarantine.len(), 2);
}

/// U-2: UTRLCLR takes the slot back, but no ABORT TASK reached the device, which may still send data for tag 0.
/// The reset is off: this is the case where the request is not proved over, so the buffers go to quarantine.
#[test]
fn a_request_cleared_without_abort_task_keeps_its_buffers() {
    use alloc::sync::Arc;
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().hang_next = true;
    let owner = Arc::new(seg(0, 4096));
    let mut quarantine = Vec::new();
    assert_eq!(
        h.read(0, 0, 1, &[*owner]),
        Err(Error::Timeout("a transfer request"))
    );
    assert_eq!(m.0.borrow().utrldbr & 1, 0, "the slot was cleared");
    assert_eq!(h.keep_held([owner.clone()], &mut quarantine), 4096);
    assert_eq!(Arc::strong_count(&owner), 2);
}

/// A fatal error mid-transfer keeps the buffers too; a request that fails with a status keeps nothing. The reset
/// is off, because this is about what a request that was not proved over leaves behind.
#[test]
fn only_requests_not_proved_over_keep_their_buffers() {
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let mut quarantine = Vec::new();
    // A completed request with a bad OCS: the controller gave the slot back and wrote the descriptor.
    m.0.borrow_mut().ocs_fail = (6, upiu::OCS_INVALID_PRDT_ATTR);
    assert!(h.read(0, 0, 1, &[seg(0, 4096)]).is_err());
    assert!(!h.is_dead());
    assert_eq!(h.keep_held([1u8], &mut quarantine), 0);
    m.0.borrow_mut().ocs_fail = (0, 0);
    // The bounce buffer is the host's own memory, never freed: nothing to keep.
    m.0.borrow_mut().hang_next = true;
    let bounce = h.bounce(4096);
    assert!(h.read(0, 0, 1, &[bounce]).is_err());
    assert_eq!(h.keep_held([2u8], &mut quarantine), 0);
    // Fatal bits.
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().hang_and_fatal = Some(regs::IS_ULLS);
    assert!(h.read(0, 0, 1, &[seg(0, 4096)]).is_err());
    assert_eq!(h.keep_held([3u8], &mut quarantine), 4096);
    assert_eq!(quarantine, [3]);
}

/// A request that takes a while: with the controller's interrupt the host sleeps on it instead of backing off, and the data
/// is the same.
#[test]
fn a_slow_request_sleeps_on_the_interrupt() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let pattern: Vec<u8> = (0..4096).map(|i| (i * 7 % 251) as u8).collect();
    m.mem_write(DATA_DADDR, &pattern);
    h.write(0, 4, 1, &[seg(0, 4096)]).unwrap();
    m.0.borrow_mut().bell_lag = 400;
    m.0.borrow_mut().irq = true;
    h.read(0, 4, 1, &[seg(0x10000, 4096)]).unwrap();
    assert!(
        m.0.borrow().irq_waits > 0,
        "the host never waited for the interrupt"
    );
    assert_eq!(m.mem_read(DATA_DADDR + 0x10000, 4096), pattern);
}

fn miss_lines(m: &Model) -> usize {
    m.0.borrow()
        .notes
        .iter()
        .filter(|n| n.starts_with("missed completion interrupt: tag "))
        .count()
}

/// A slow request whose interrupt comes: every sleep is capped at 20 ms, and nothing is counted as missed.
#[test]
fn a_request_with_its_interrupt_is_not_a_miss() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().bell_lag = 400;
    m.0.borrow_mut().irq = true;
    h.read(0, 4, 1, &[seg(0x10000, 4096)]).unwrap();
    let s = m.0.borrow();
    assert!(s.irq_waits > 0);
    assert!(s.irq_wait_max_us <= crate::irq_wait::SLEEP_CAP_US);
    assert_eq!((s.missed, h.missed_irq(), h.late_ms_max()), (0, 0, 0));
    drop(s);
    assert_eq!(miss_lines(&m), 0);
}

/// The completion interrupt is lost: the request is found done after one capped sleep instead of at the 30 s timeout,
/// completes with its data, and is counted and logged once.
#[test]
fn a_lost_interrupt_is_found_after_one_capped_sleep() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    let pattern: Vec<u8> = (0..4096).map(|i| (i * 13 % 251) as u8).collect();
    m.mem_write(DATA_DADDR, &pattern);
    h.write(0, 4, 1, &[seg(0, 4096)]).unwrap();
    {
        let mut s = m.0.borrow_mut();
        // Done 66 looks after the bell: just after the 64 quick looks, during the first sleep.
        s.bell_lag = 66;
        s.irq = true;
        s.irq_lost = true;
    }
    h.read(0, 4, 1, &[seg(0x10000, 4096)]).unwrap();
    assert_eq!(m.mem_read(DATA_DADDR + 0x10000, 4096), pattern);
    let s = m.0.borrow();
    assert!(s.irq_wait_max_us <= crate::irq_wait::SLEEP_CAP_US);
    assert!(
        s.irq_waits == 1,
        "{} sleeps for one lost interrupt",
        s.irq_waits
    );
    assert_eq!(s.missed, 1);
    let line = s
        .notes
        .iter()
        .find(|n| n.starts_with("missed completion interrupt: "))
        .unwrap()
        .clone();
    drop(s);
    assert_eq!(h.missed_irq(), 1);
    assert!(
        h.late_ms_max() >= 20 && h.late_ms_max() <= 21,
        "{}",
        h.late_ms_max()
    );
    assert_eq!(
        line,
        alloc::format!(
            "missed completion interrupt: tag 0, {} ms late, IS=0x1",
            h.late_ms_max()
        )
    );
    assert_eq!(miss_lines(&m), 1);
}

/// A request that never finishes still times out at 30 s, as before the sleeps were capped; no miss is counted.
/// The reset is off, so the 30 s of waiting on the interrupt is the whole of what the request costs.
#[test]
fn a_hung_request_with_a_lost_interrupt_times_out_at_30_s() {
    let m = Model::cold(laptop_lus());
    let mut h = start_no_recover(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.irq = true;
        s.irq_lost = true;
        s.hang_next = true;
    }
    assert_eq!(
        h.read(0, 0, 1, &[seg(0, 4096)]),
        Err(Error::Timeout("a transfer request"))
    );
    assert_eq!(h.read(0, 1, 1, &[seg(0, 4096)]), Err(Error::Dead));
    let s = m.0.borrow();
    // 30 s in sleeps of at most 20 ms each, and not one more than the timeout needs.
    assert!(
        s.irq_waits >= 1500 && s.irq_waits <= 1502,
        "{}",
        s.irq_waits
    );
    assert!(s.irq_wait_max_us <= crate::irq_wait::SLEEP_CAP_US);
    assert_eq!((s.missed, h.missed_irq()), (0, 0));
}

/// Many lost interrupts: each is counted, the log gets 10 lines a second, then one "N more" line.
#[test]
fn lost_interrupts_are_all_counted_and_the_log_is_rate_limited() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    {
        let mut s = m.0.borrow_mut();
        s.bell_lag = 66;
        s.irq = true;
        s.irq_lost = true;
    }
    // One 20 ms sleep each: 15 requests are within the first second.
    for i in 0..15 {
        h.read(0, i, 1, &[seg(0, 4096)]).unwrap();
    }
    assert_eq!(h.missed_irq(), 15);
    assert_eq!(miss_lines(&m), 10);
    // Past the first second: the next window says how many were not logged.
    for i in 0..60 {
        h.read(0, i % 16, 1, &[seg(0, 4096)]).unwrap();
    }
    assert_eq!(h.missed_irq(), 75);
    assert_eq!(m.0.borrow().missed, 75);
    let more: Vec<_> =
        m.0.borrow()
            .notes
            .iter()
            .filter(|n| {
                n.starts_with("missed completion interrupt: ") && n.ends_with(" more not logged")
            })
            .cloned()
            .collect();
    assert!(!more.is_empty());
    // Each window logged its 10 lines; the misses of a full window past those are said in the next window's "N more"
    // line, and those of the window still open are only counted.
    let said: u64 = more
        .iter()
        .map(|n| n.split(' ').nth(3).unwrap().parse::<u64>().unwrap())
        .sum();
    assert!(miss_lines(&m) as u64 + said <= 75);
    assert_eq!(miss_lines(&m), 10 * (more.len() + 1));
}

/// Without an interrupt (QEMU's PCI controller), the host polls as before: no sleeps, no misses.
#[test]
fn a_polled_request_has_no_misses() {
    let m = Model::cold(laptop_lus());
    let mut h = start(&m, ResetPolicy::IfNeeded, WritePolicy::default()).unwrap();
    m.0.borrow_mut().bell_lag = 400;
    h.read(0, 4, 1, &[seg(0x10000, 4096)]).unwrap();
    let s = m.0.borrow();
    assert_eq!((s.irq_waits, s.missed, h.missed_irq()), (0, 0, 0));
}
