//! The RSC driver against a software model of the register file (tests/common/sim.rs), which was written from the
//! register semantics and not from the driver, and the model itself against hand-written wrong sequences.

mod common;

use common::CountDelay;
use common::sim::SimRsc;
use lucina_modem_rpmh::rsc::{Rsc, RscConfig, RscError, TcsCmd, TcsRequest};
use lucina_qcom_soc::io::RegIo;

const A: u32 = 0x30010;
const B: u32 = 0x30020;
const C: u32 = 0x30030;
const VRM_A: u32 = 0x40900;

fn cmd(addr: u32, data: u32, wait: bool) -> TcsCmd {
    TcsCmd { addr, data, wait }
}

fn setup() -> (SimRsc, RscConfig) {
    (
        SimRsc::galaxy_book_go(),
        RscConfig {
            bus: None,
            ..RscConfig::GALAXY_BOOK_GO_DRV2
        },
    )
}

#[test]
fn probe_reads_the_version_and_arms_the_active_tcses() {
    let (sim, cfg) = setup();
    let rsc = Rsc::probe(&sim, &cfg).unwrap();
    assert_eq!(rsc.version(), (2, 7));
    assert_eq!(rsc.num_tcs(), 8);
    assert_eq!(sim.irq_enable(), 0b11, "IRQ_ENABLE holds the ACTIVE mask");
    sim.assert_clean();
}

#[test]
fn probe_refuses_layout_3_and_bad_geometry() {
    let cfg = RscConfig {
        bus: None,
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    for id in [0x0003_0000, 0x0003_0700, 0x00ff_0000] {
        let sim = SimRsc::new(id, 2, 8, 3, 0xd00, 0x10000);
        let e = Rsc::probe(&sim, &cfg).err().unwrap();
        assert!(
            matches!(e, RscError::UnsupportedVersion { major, .. } if major >= 3),
            "{e:?}"
        );
    }
    // 2.x and 1.x are layout 2.7 in Linux
    for id in [0x0002_0000, 0x0001_0100, 0x0000_0000] {
        let sim = SimRsc::new(id, 2, 8, 3, 0xd00, 0x10000);
        assert!(Rsc::probe(&sim, &cfg).is_ok(), "{id:#x}");
    }
    // commands per TCS of 0
    let sim = SimRsc::new(0x0002_0700, 2, 8, 0, 0xd00, 0x10000);
    assert!(matches!(
        Rsc::probe(&sim, &cfg),
        Err(RscError::BadConfig(_))
    ));
    // the window does not reach the last TCS
    let sim = SimRsc::new(0x0002_0700, 2, 8, 3, 0xd00, 0x1000);
    let small = RscConfig {
        window: 0x1000,
        ..cfg
    };
    assert!(matches!(
        Rsc::probe(&sim, &small),
        Err(RscError::BadConfig(_))
    ));
    // a DRV number that does not fit in the config word
    let sim = SimRsc::new(0x0002_0700, 2, 8, 3, 0xd00, 0x10000);
    let far = RscConfig { drv_id: 6, ..cfg };
    assert!(matches!(
        Rsc::probe(&sim, &far),
        Err(RscError::BadConfig(_))
    ));
    // nothing was written by the refusals except possibly nothing at all
}

#[test]
fn a_vote_is_one_clean_transfer() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    rsc.send_cmd(&cmd(A, 7, true), &CountDelay::default(), 10)
        .unwrap();
    sim.assert_clean();
    let ex = sim.executed();
    assert_eq!(ex.len(), 1);
    assert_eq!(ex[0].tcs, 0);
    // MSGID: length 8 | response required | write
    assert_eq!(ex[0].cmds, vec![(0x10108, A, 7)]);
    // reclaimed: nothing triggered, no commands left, status bit cleared, TCS free again
    assert!(!sim.triggered(0));
    assert_eq!(sim.cmd_enable(0), 0);
    assert_eq!(sim.irq_status(), 0);
    assert_eq!(rsc.in_use(), 0);
}

#[test]
fn an_async_vote_asks_for_no_response_but_still_waits_for_the_tcs() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    rsc.send_cmd(&cmd(A, 7, false), &CountDelay::default(), 10)
        .unwrap();
    sim.assert_clean();
    assert_eq!(sim.executed()[0].cmds, vec![(0x10008, A, 7)]);
    assert_eq!(rsc.in_use(), 0);
}

#[test]
fn a_command_can_ask_for_a_response_in_a_request_that_does_not() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let cmds = [
        cmd(VRM_A, 488, false),
        cmd(VRM_A + 4, 1, true),
        cmd(VRM_A + 8, 7, false),
    ];
    let req = TcsRequest {
        wait_for_compl: false,
        cmds: &cmds,
    };
    rsc.send(&req, &CountDelay::default(), 10).unwrap();
    sim.assert_clean();
    let got: Vec<u32> = sim.executed()[0].cmds.iter().map(|c| c.0).collect();
    assert_eq!(got, vec![0x10008, 0x10108, 0x10008]);
    // and all three go to one TCS, which the model accepts although they are one VRM resource: they are one request
}

#[test]
fn many_votes_reuse_the_tcs() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    for i in 0..40 {
        rsc.send_cmd(
            &cmd([A, B, C, VRM_A][i % 4], i as u32, i % 3 == 0),
            &CountDelay::default(),
            10,
        )
        .unwrap();
    }
    sim.assert_clean();
    assert_eq!(sim.executed().len(), 40);
    assert!(
        sim.executed().iter().all(|e| e.tcs == 0),
        "always the first free TCS"
    );
}

#[test]
fn it_spins_first_and_sleeps_after() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let d = CountDelay::default();
    sim.set(|s| s.latency = 100);
    rsc.send_cmd(&cmd(A, 1, true), &d, 10).unwrap();
    assert_eq!(d.0.get(), 0, "a quick answer needs no sleep");
    sim.set(|s| s.latency = 3000);
    rsc.send_cmd(&cmd(A, 1, true), &d, 5000).unwrap();
    assert!(d.0.get() > 0, "a slow answer sleeps");
    sim.assert_clean();
}

#[test]
fn a_timeout_keeps_the_tcs_claimed_until_it_is_reaped() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let d = CountDelay::default();
    sim.set(|s| s.stuck = true);
    assert_eq!(
        rsc.send_cmd(&cmd(A, 1, true), &d, 3),
        Err(RscError::Timeout { tcs: 0 })
    );
    assert_eq!(d.0.get(), 3, "slept for the whole timeout");
    // another address takes the other ACTIVE TCS
    assert_eq!(
        rsc.send_cmd(&cmd(B, 1, true), &d, 0),
        Err(RscError::Timeout { tcs: 1 })
    );
    // the same address as an in-flight one, and then no TCS at all
    assert_eq!(rsc.send_cmd(&cmd(A, 2, true), &d, 0), Err(RscError::Busy));
    assert_eq!(rsc.send_cmd(&cmd(C, 2, true), &d, 0), Err(RscError::Busy));
    assert_eq!(rsc.in_use(), 0b11);
    // the model saw only the two transfers; the refused ones never reached it
    assert_eq!(sim.executed().len(), 2);
    sim.assert_clean();
    // the answers arrive late
    sim.set(|s| s.stuck = false);
    sim.complete_all();
    assert_eq!(rsc.reap(), 2);
    assert_eq!(rsc.in_use(), 0);
    assert_eq!(rsc.reap(), 0);
    rsc.send_cmd(&cmd(A, 3, true), &d, 0).unwrap();
    sim.assert_clean();
}

#[test]
fn two_registers_of_one_vrm_resource_conflict_but_two_arc_addresses_do_not() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let d = CountDelay::default();
    sim.set(|s| s.stuck = true);
    assert!(matches!(
        rsc.send_cmd(&cmd(VRM_A, 1, true), &d, 0),
        Err(RscError::Timeout { .. })
    ));
    // +4 is the enable register of the same resource
    assert_eq!(
        rsc.send_cmd(&cmd(VRM_A + 4, 1, true), &d, 0),
        Err(RscError::Busy)
    );
    // the next resource is another one
    assert!(matches!(
        rsc.send_cmd(&cmd(VRM_A + 0x10, 1, true), &d, 0),
        Err(RscError::Timeout { tcs: 1 })
    ));
    sim.assert_clean();

    let sim = SimRsc::galaxy_book_go();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    sim.set(|s| s.stuck = true);
    assert!(matches!(
        rsc.send_cmd(&cmd(A, 1, true), &d, 0),
        Err(RscError::Timeout { .. })
    ));
    assert!(
        matches!(
            rsc.send_cmd(&cmd(A + 4, 1, true), &d, 0),
            Err(RscError::Timeout { tcs: 1 })
        ),
        "ARC addresses differ"
    );
    sim.assert_clean();
}

#[test]
fn without_an_active_group_the_wake_tcs_is_borrowed_cleanly() {
    let sim = SimRsc::galaxy_book_go();
    // the model only latches a status bit for a TCS whose interrupt is enabled: a driver that forgets fails
    sim.set(|s| s.status_needs_enable = true);
    let cfg = RscConfig {
        tcs_config: [(2, 0), (0, 3), (1, 3), (3, 1)],
        bus: None,
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    assert_eq!(sim.irq_enable(), 0, "no ACTIVE TCS to arm");
    // stale wake commands in the first WAKE TCS (number 3)
    sim.plant_commands(3, 0b111);
    rsc.send_cmd(&cmd(A, 5, true), &CountDelay::default(), 10)
        .unwrap();
    sim.assert_clean();
    let ex = sim.executed();
    assert_eq!(ex[0].tcs, 3);
    assert_eq!(ex[0].cmds.len(), 1, "the stale commands were not triggered");
    assert_eq!(
        sim.irq_enable(),
        0,
        "the interrupt bit is switched off again"
    );
    assert_eq!(sim.cmd_enable(3), 0);
}

#[test]
fn no_active_and_no_wake_tcs_is_an_error() {
    let sim = SimRsc::galaxy_book_go();
    let cfg = RscConfig {
        tcs_config: [(2, 0), (0, 3), (1, 0), (3, 1)],
        bus: None,
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    assert_eq!(
        rsc.send_cmd(&cmd(A, 1, true), &CountDelay::default(), 1),
        Err(RscError::NoActiveTcs)
    );
}

#[test]
fn bad_requests_are_refused_before_any_register_is_touched() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let d = CountDelay::default();
    let none = TcsRequest {
        wait_for_compl: true,
        cmds: &[],
    };
    assert_eq!(rsc.send(&none, &d, 1), Err(RscError::EmptyRequest));
    let four = [cmd(A, 0, false); 4];
    let req = TcsRequest {
        wait_for_compl: true,
        cmds: &four,
    };
    assert_eq!(rsc.send(&req, &d, 1), Err(RscError::TooManyCmds { max: 3 }));
    assert_eq!(
        rsc.send_cmd(&cmd(0, 1, true), &d, 1),
        Err(RscError::BadAddress { index: 0 })
    );
    let two = [cmd(A, 0, false), cmd(0, 0, false)];
    let req = TcsRequest {
        wait_for_compl: true,
        cmds: &two,
    };
    assert_eq!(
        rsc.send(&req, &d, 1),
        Err(RscError::BadAddress { index: 1 })
    );
    assert!(sim.executed().is_empty());
    assert_eq!(rsc.in_use(), 0);
    sim.assert_clean();
}

#[test]
fn a_register_that_does_not_read_back_is_reported() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    sim.set(|s| s.control_lag = u32::MAX);
    let e = rsc
        .send_cmd(&cmd(A, 1, true), &CountDelay::default(), 1)
        .unwrap_err();
    assert!(matches!(e, RscError::RegisterStuck { tcs: 0, .. }), "{e:?}");
}

#[test]
fn a_command_that_was_not_acknowledged_is_reported_after_the_tcs_is_freed() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    sim.set(|s| s.ack_resp = false);
    let e = rsc
        .send_cmd(&cmd(A, 1, true), &CountDelay::default(), 1)
        .unwrap_err();
    assert_eq!(
        e,
        RscError::CmdNotAcked {
            tcs: 0,
            cmd: 0,
            status: 0x100
        }
    );
    assert_eq!(rsc.in_use(), 0, "the TCS is reclaimed either way");
    assert_eq!(sim.irq_status(), 0);
    sim.assert_clean();
    // an async request needs no COMPL, so the same hardware answer is fine
    rsc.send_cmd(&cmd(A, 1, false), &CountDelay::default(), 1)
        .unwrap();
    // and the check can be switched off
    rsc.quirks.check_cmd_status = false;
    rsc.send_cmd(&cmd(A, 1, true), &CountDelay::default(), 1)
        .unwrap();
}

#[test]
fn the_wait_for_cmpl_quirk_writes_the_response_mask_and_clears_it() {
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    rsc.quirks.write_wait_for_cmpl = true;
    sim.set(|s| s.stuck = true);
    let cmds = [cmd(A, 1, false), cmd(B, 1, true), cmd(C, 1, false)];
    let req = TcsRequest {
        wait_for_compl: false,
        cmds: &cmds,
    };
    assert!(matches!(
        rsc.send(&req, &CountDelay::default(), 0),
        Err(RscError::Timeout { .. })
    ));
    assert_eq!(sim.wait_for_cmpl(0), 0b010);
    sim.set(|s| s.stuck = false);
    sim.complete_all();
    assert_eq!(rsc.reap(), 1);
    assert_eq!(sim.wait_for_cmpl(0), 0);
    sim.assert_clean();
    // off by default
    let (sim, cfg) = setup();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    sim.set(|s| s.stuck = true);
    let _ = rsc.send_cmd(&cmd(A, 1, true), &CountDelay::default(), 0);
    assert_eq!(sim.wait_for_cmpl(0), 0);
}

// ---- the model itself: does it refuse what the hardware would? ----

const BASE: u32 = 0xd00;
const T0: u32 = BASE;
const T1: u32 = BASE + 672;
const CONTROL: u32 = 0x14;
const ENABLE: u32 = 0x1c;
const MSGID: u32 = 0x30;
const ADDR: u32 = 0x34;
const DATA: u32 = 0x38;
const EN: u32 = 1 << 16;
const TRIG: u32 = 1 << 24;

fn write_cmd(sim: &SimRsc, t: u32, j: u32, msgid: u32, addr: u32) {
    sim.write32(t + MSGID + 20 * j, msgid);
    sim.write32(t + ADDR + 20 * j, addr);
    sim.write32(t + DATA + 20 * j, 1);
}

fn has(sim: &SimRsc, needle: &str) -> bool {
    sim.errors().iter().any(|e| e.contains(needle))
}

#[test]
fn the_model_accepts_the_right_sequence() {
    let sim = SimRsc::galaxy_book_go();
    sim.set(|s| s.latency = 3);
    write_cmd(&sim, T0, 0, 0x10108, A);
    sim.write32(T0 + ENABLE, 1);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert_eq!(sim.read32(BASE + 4) & 1, 0, "not done on the first look");
    for _ in 0..2 {
        sim.read32(BASE + 4);
    }
    assert_eq!(sim.irq_status(), 1);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, 0);
    sim.write32(T0 + ENABLE, 0);
    sim.write32(BASE + 8, 1);
    sim.assert_clean();
    assert_eq!(sim.irq_status(), 0);
}

#[test]
fn the_model_rejects_a_trigger_while_the_tcs_is_not_free() {
    let sim = SimRsc::galaxy_book_go();
    sim.set(|s| s.stuck = true);
    write_cmd(&sim, T0, 0, 0x10108, A);
    sim.write32(T0 + ENABLE, 1);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    sim.assert_clean();
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert!(has(&sim, "triggered again"), "{:?}", sim.errors());
}

#[test]
fn the_model_rejects_writing_commands_into_a_triggered_tcs() {
    let sim = SimRsc::galaxy_book_go();
    sim.set(|s| s.stuck = true);
    write_cmd(&sim, T0, 0, 0x10108, A);
    sim.write32(T0 + ENABLE, 1);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    sim.write32(T0 + ADDR, B);
    assert!(
        has(&sim, "written while the TCS is triggered"),
        "{:?}",
        sim.errors()
    );
}

#[test]
fn the_model_rejects_a_missing_enable() {
    let sim = SimRsc::galaxy_book_go();
    write_cmd(&sim, T0, 0, 0x10108, A);
    // CMD_ENABLE never written
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert!(has(&sim, "no command enabled"), "{:?}", sim.errors());
    assert!(has(&sim, "written but not enabled"), "{:?}", sim.errors());
}

#[test]
fn the_model_rejects_an_enable_for_a_command_that_was_not_written() {
    let sim = SimRsc::galaxy_book_go();
    write_cmd(&sim, T0, 0, 0x10108, A);
    sim.write32(T0 + ENABLE, 0b11);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert!(
        has(&sim, "enabled but were not written"),
        "{:?}",
        sim.errors()
    );
}

#[test]
fn the_model_rejects_address_0_and_a_bad_msgid() {
    let sim = SimRsc::galaxy_book_go();
    write_cmd(&sim, T0, 0, 0x10108, 0);
    write_cmd(&sim, T0, 1, 0x108, A);
    sim.write32(T0 + ENABLE, 0b11);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert!(has(&sim, "command 0 has address 0"), "{:?}", sim.errors());
    assert!(has(&sim, "command 1 MSGID"), "{:?}", sim.errors());
}

#[test]
fn the_model_rejects_trigger_without_a_prior_enable_and_a_wrong_clear_order() {
    let sim = SimRsc::galaxy_book_go();
    write_cmd(&sim, T0, 0, 0x10108, A);
    sim.write32(T0 + ENABLE, 1);
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert!(
        has(
            &sim,
            "without AMC mode having been enabled by an earlier write"
        ),
        "{:?}",
        sim.errors()
    );
    sim.complete_all();
    // clearing the enable and the trigger together
    sim.write32(T0 + CONTROL, 0);
    assert!(
        has(&sim, "enable cleared in the same write as the trigger"),
        "{:?}",
        sim.errors()
    );
    // a trigger without the enable
    let sim = SimRsc::galaxy_book_go();
    sim.write32(T0 + CONTROL, TRIG);
    assert!(
        has(&sim, "TRIGGER set without AMC mode enabled"),
        "{:?}",
        sim.errors()
    );
}

#[test]
fn the_model_rejects_the_same_address_in_two_tcses() {
    let sim = SimRsc::galaxy_book_go();
    sim.set(|s| s.stuck = true);
    write_cmd(&sim, T0, 0, 0x10108, VRM_A);
    sim.write32(T0 + ENABLE, 1);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    write_cmd(&sim, T1, 0, 0x10108, VRM_A + 4);
    sim.write32(T1 + ENABLE, 1);
    sim.write32(T1 + CONTROL, EN);
    sim.write32(T1 + CONTROL, EN | TRIG);
    assert!(has(&sim, "which TCS 0 has in flight"), "{:?}", sim.errors());
}

#[test]
fn the_model_rejects_stale_commands() {
    let sim = SimRsc::galaxy_book_go();
    sim.plant_commands(0, 0b100);
    write_cmd(&sim, T0, 0, 0x10108, A);
    // the driver ORs its bit into whatever CMD_ENABLE held, stale bit 2 included
    sim.write32(T0 + ENABLE, 0b101);
    sim.write32(T0 + CONTROL, EN);
    sim.write32(T0 + CONTROL, EN | TRIG);
    assert!(
        has(&sim, "commands 0x4 are enabled but were not written"),
        "{:?}",
        sim.errors()
    );
}

#[test]
fn the_model_rejects_unmodelled_accesses() {
    let sim = SimRsc::galaxy_book_go();
    sim.read32(0x10);
    sim.write32(0x0c, 1);
    sim.write32(T0 + 0x18, 1);
    sim.read32(0x20000);
    assert_eq!(sim.errors().len(), 4, "{:?}", sim.errors());
}

/// Two drivers on one DRV (the kernel builds one in the modem, audio, Venus and the SD rails): with one `Bus` a TCS that
/// one of them left claimed after a timeout is claimed for the other too, as the one `drv->tcs_in_use` of Linux has it, so
/// the other takes the second ACTIVE TCS instead of writing over the first.
#[test]
fn drivers_on_one_bus_share_the_in_use_mask_and_the_lock() {
    static BUS: lucina_modem_rpmh::rsc::Bus = lucina_modem_rpmh::rsc::Bus::new();
    let sim = SimRsc::galaxy_book_go();
    let cfg = RscConfig {
        bus: Some(&BUS),
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    let mut modem = Rsc::probe(&sim, &cfg).unwrap();
    let mut venus = Rsc::probe(&sim, &cfg).unwrap();
    let d = CountDelay::default();
    sim.set(|s| s.stuck = true);
    assert_eq!(
        modem.send_cmd(&cmd(A, 1, true), &d, 1),
        Err(RscError::Timeout { tcs: 0 })
    );
    assert_eq!(venus.in_use(), 0b1, "the other driver sees TCS 0 claimed");
    assert_eq!(
        venus.send_cmd(&cmd(B, 1, true), &d, 0),
        Err(RscError::Timeout { tcs: 1 })
    );
    assert_eq!(modem.send_cmd(&cmd(C, 1, true), &d, 0), Err(RscError::Busy));
    assert!(!BUS.is_busy(), "the lock is given back after an error");
    sim.set(|s| s.stuck = false);
    sim.complete_all();
    // Either driver may reap: the mask is one.
    assert_eq!(venus.reap(), 2);
    assert_eq!(modem.in_use(), 0);
    modem.send_cmd(&cmd(A, 2, true), &d, 0).unwrap();
    venus.send_cmd(&cmd(B, 2, true), &d, 0).unwrap();
    assert_eq!(BUS.in_use(), 0);
    sim.assert_clean();
}

/// Without a bus each driver keeps its own mask, as before, and as a test of one driver wants.
#[test]
fn drivers_without_a_bus_do_not_see_each_other() {
    let (sim, cfg) = setup();
    let mut one = Rsc::probe(&sim, &cfg).unwrap();
    let two = Rsc::probe(&sim, &cfg).unwrap();
    sim.set(|s| s.stuck = true);
    assert_eq!(
        one.send_cmd(&cmd(A, 1, true), &CountDelay::default(), 0),
        Err(RscError::Timeout { tcs: 0 })
    );
    assert_eq!(two.in_use(), 0);
}

/// The model behind a lock of its own, so that a [`Window`] (which must be `Send`) can hold it and the test still reads it.
#[derive(Clone)]
struct SharedSim(std::sync::Arc<std::sync::Mutex<SimRsc>>);

impl RegIo for SharedSim {
    fn read32(&self, off: u32) -> u32 {
        self.0.lock().unwrap().read32(off)
    }

    fn write32(&self, off: u32, value: u32) {
        self.0.lock().unwrap().write32(off, value)
    }
}

/// Real sleeps, counted, for the threads of the `Drv` tests.
#[derive(Default)]
struct ThreadDelay(std::sync::atomic::AtomicU32);

impl lucina_qcom_soc::io::Delay for ThreadDelay {
    fn sleep_ms(&self, ms: u32) {
        self.0.fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
    }
}

/// `rpmh_rsc_probe` once for the whole kernel: the first caller's probe makes the one `Rsc`, a later caller's probe is not run, and
/// before any probe there is nothing to send through.
#[test]
fn the_one_drv_is_probed_once() {
    use lucina_modem_rpmh::rsc::{Drv, Window};
    let drv = Drv::new();
    let d = ThreadDelay::default();
    assert!(drv.with(&d, |rsc| rsc.version()).is_none());
    let sim = SharedSim(std::sync::Arc::new(std::sync::Mutex::new(
        SimRsc::galaxy_book_go(),
    )));
    let cfg = RscConfig {
        bus: None,
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    let first = sim.clone();
    drv.open(&d, || Rsc::probe(Window(Box::new(first)), &cfg))
        .unwrap();
    let again: Result<(), RscError> = drv.open(&d, || panic!("a second probe of the one DRV"));
    again.unwrap();
    assert_eq!(drv.with(&d, |rsc| rsc.version()), Some((2, 7)));
    drv.with(&d, |rsc| rsc.send_cmd(&cmd(A, 7, true), &d, 10))
        .unwrap()
        .unwrap();
    let sim = sim.0.lock().unwrap();
    sim.assert_clean();
    assert_eq!(sim.executed().len(), 1);
}

/// A failed probe leaves it unopened, and the next caller probes.
#[test]
fn a_failed_probe_leaves_the_drv_to_the_next_caller() {
    use lucina_modem_rpmh::rsc::{Drv, Window};
    let drv = Drv::new();
    let d = ThreadDelay::default();
    let failed: Result<(), RscError> = drv.open(&d, || Err(RscError::BadConfig("no window")));
    assert_eq!(failed, Err(RscError::BadConfig("no window")));
    assert!(drv.with(&d, |_| ()).is_none());
    let sim = SharedSim(std::sync::Arc::new(std::sync::Mutex::new(
        SimRsc::galaxy_book_go(),
    )));
    let cfg = RscConfig {
        bus: None,
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    drv.open(&d, || Rsc::probe(Window(Box::new(sim)), &cfg))
        .unwrap();
    assert!(drv.with(&d, |_| ()).is_some());
}

/// One transaction at a time: a driver that comes while another holds the lock sleeps (it does not spin) until the first lets go,
/// and the transactions of both run whole and in turn on the one DRV.
#[test]
fn transactions_of_two_drivers_wait_for_each_other() {
    use std::sync::atomic::{AtomicBool, Ordering};

    use lucina_modem_rpmh::rsc::{Drv, Window};
    static DRV: Drv = Drv::new();
    static INSIDE: AtomicBool = AtomicBool::new(false);
    let sim = SharedSim(std::sync::Arc::new(std::sync::Mutex::new(
        SimRsc::galaxy_book_go(),
    )));
    let cfg = RscConfig {
        bus: None,
        ..RscConfig::GALAXY_BOOK_GO_DRV2
    };
    let first = sim.clone();
    DRV.open(&ThreadDelay::default(), || {
        Rsc::probe(Window(Box::new(first)), &cfg)
    })
    .unwrap();
    let threads: Vec<_> = [A, B]
        .into_iter()
        .map(|addr| {
            std::thread::spawn(move || {
                let d = ThreadDelay::default();
                for n in 0..20 {
                    DRV.with(&d, |rsc| {
                        assert!(
                            !INSIDE.swap(true, Ordering::SeqCst),
                            "two transactions at once"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        let sent = rsc.send_cmd(&cmd(addr, n, true), &d, 10);
                        INSIDE.store(false, Ordering::SeqCst);
                        sent
                    })
                    .unwrap()
                    .unwrap();
                }
                d.0.load(Ordering::Relaxed)
            })
        })
        .collect();
    let slept: u32 = threads.into_iter().map(|t| t.join().unwrap()).sum();
    assert!(slept > 0, "the second driver waited by sleeping");
    let sim = sim.0.lock().unwrap();
    sim.assert_clean();
    assert_eq!(sim.executed().len(), 40);
    assert!(
        sim.executed().iter().all(|e| e.tcs == 0),
        "each request found TCS 0 free again"
    );
}
