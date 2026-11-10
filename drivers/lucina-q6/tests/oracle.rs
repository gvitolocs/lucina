//! A whole playback against a model of the DSP written from Linux's callbacks, with every packet compared to what Linux's own
//! q6afe.c, q6adm.c, q6asm.c and q6core.c sent for the same stream (`oracle/`).

use lucina_q6::apr::{self, BASIC_RSP_RESULT, DOMAIN_ADSP, HDR_SIZE, SEQ_CMD_HDR_FIELD};
use lucina_q6::avcs;
use lucina_q6::stream::{Config, Event, Outcome, Playback, SPEAKERS, Step};

const HANDLE: u32 = 0xabcd_1234;
const COPP: u16 = 0x11;

fn config() -> Config {
    Config {
        phys: 0x1_2345_6000,
        period_bytes: 3840,
        periods: 8,
        ..SPEAKERS
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let text = include_str!("fixtures/oracle.txt");
    let hex = text
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("no fixture {name}"));
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

/// A packet from the DSP, to the port `dest_port`.
fn from_dsp_to(svc: u8, dest_port: u16, token: u32, opcode: u32, payload: &[u8]) -> Vec<u8> {
    let size = HDR_SIZE + payload.len();
    let mut out = Vec::new();
    out.extend_from_slice(&SEQ_CMD_HDR_FIELD.to_le_bytes());
    out.extend_from_slice(&(size as u16).to_le_bytes());
    out.push(svc);
    out.push(DOMAIN_ADSP);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(svc);
    out.push(apr::DOMAIN_APPS);
    out.extend_from_slice(&dest_port.to_le_bytes());
    out.extend_from_slice(&token.to_le_bytes());
    out.extend_from_slice(&opcode.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

fn from_dsp(svc: u8, token: u32, opcode: u32, payload: &[u8]) -> Vec<u8> {
    from_dsp_to(svc, 0, token, opcode, payload)
}

fn basic(svc: u8, opcode: u32, status: u32) -> Vec<u8> {
    basic_to(svc, 0, 0, opcode, status)
}

fn basic_to(svc: u8, dest_port: u16, token: u32, opcode: u32, status: u32) -> Vec<u8> {
    let mut p = opcode.to_le_bytes().to_vec();
    p.extend_from_slice(&status.to_le_bytes());
    from_dsp_to(svc, dest_port, token, BASIC_RSP_RESULT, &p)
}

/// What the model DSP answers to a command: `q6afe_callback`, `q6adm_callback`, `q6asm_srvc_callback`.
fn answer(command: &[u8], status: u32) -> Vec<u8> {
    // Not apr::parse: that is for what comes from the DSP, and a command can be a bare header.
    struct Command {
        src_svc: u8,
        src_port: u16,
        token: u32,
        opcode: u32,
    }
    let word = |at: usize| u32::from_le_bytes(command[at..at + 4].try_into().unwrap());
    let packet = Command {
        src_svc: command[4],
        src_port: u16::from_le_bytes(command[6..8].try_into().unwrap()),
        token: word(12),
        opcode: word(16),
    };
    match packet.opcode {
        0x0001_0D92 if status == 0 => from_dsp_to(
            apr::SVC_ASM,
            packet.src_port,
            packet.token,
            0x0001_0D93,
            &HANDLE.to_le_bytes(),
        ),
        0x0001_0326 if status == 0 => {
            let mut p = 0u32.to_le_bytes().to_vec();
            p.extend_from_slice(&COPP.to_le_bytes());
            p.extend_from_slice(&0u16.to_le_bytes());
            from_dsp_to(apr::SVC_ADM, packet.src_port, packet.token, 0x0001_0329, &p)
        }
        opcode => basic_to(
            packet.src_svc,
            packet.src_port,
            packet.token,
            opcode,
            status,
        ),
    }
}

fn feed(playback: &mut Playback, bytes: &[u8]) -> Outcome {
    playback.on_packet(&apr::parse(bytes).unwrap())
}

#[test]
fn a_playback_says_what_linux_says_in_linuxs_order() {
    let mut playback = Playback::new(config());
    let mut command = playback.start();
    for name in [
        "clock_on",
        "port_config",
        "port_start",
        "map_memory",
        "open_write",
        "adm_open",
        "matrix",
        "media_format",
    ] {
        assert_eq!(hex(&command), hex(&fixture(name)), "{name}");
        assert!(playback.is_waiting());
        let outcome = feed(&mut playback, &answer(&command, 0));
        match outcome.send {
            Some(next) => command = next,
            None => {
                assert_eq!(name, "media_format");
                assert_eq!(outcome.event, Event::Prepared);
            }
        }
    }
    assert_eq!(playback.step(), Step::Prepared);
    assert!(!playback.is_waiting());

    // q6asm_dai_ack queues the first periods before the trigger.
    for slot in 0..3 {
        assert_eq!(playback.next_slot(), slot);
        assert_eq!(
            hex(&playback.write(3840).unwrap()),
            hex(&fixture(&format!("write_slot{slot}")))
        );
    }
    let run = playback.run().unwrap();
    assert_eq!(hex(&run), hex(&fixture("run")));
    assert!(playback.is_waiting());
    assert_eq!(feed(&mut playback, &answer(&run, 0)).event, Event::Running);
    assert_eq!(playback.step(), Step::Running);
    assert_eq!(
        hex(&playback.write(3840).unwrap()),
        hex(&fixture("write_slot3"))
    );

    // What Linux's q6asm_stream_callback accepted for slot 2.
    let done = from_dsp_to(
        apr::SVC_ASM,
        0x0101,
        2 | (3840 << 16),
        0x0001_0D99,
        &fixture("write_done_slot2"),
    );
    assert_eq!(
        feed(&mut playback, &done).event,
        Event::WriteDone { slot: 2 }
    );

    let mut down = playback.stop();
    assert_eq!(hex(&down[0]), hex(&fixture("eos")));
    let mut command = down.pop().unwrap();
    assert_eq!(down.len(), 1);
    for name in ["port_stop", "clock_off", "close", "unmap", "adm_close"] {
        assert_eq!(hex(&command), hex(&fixture(name)), "{name}");
        let outcome = feed(&mut playback, &answer(&command, 0));
        match outcome.send {
            Some(next) => command = next,
            None => {
                assert_eq!(name, "adm_close");
                assert_eq!(outcome.event, Event::Stopped);
            }
        }
    }
    assert_eq!(playback.step(), Step::Stopped);
    assert_eq!(playback.write(3840), None);
}

#[test]
fn a_write_done_for_another_address_proves_nothing() {
    let mut playback = prepared();
    let wrong = from_dsp_to(apr::SVC_ASM, 0x0101, 2 | (3840 << 16), 0x0001_0D99, &[0; 8]);
    assert!(matches!(
        feed(&mut playback, &wrong).event,
        Event::WrongWriteDone { slot: 2, .. }
    ));
    // A slot that does not exist.
    let wrong = from_dsp_to(
        apr::SVC_ASM,
        0x0101,
        9,
        0x0001_0D99,
        &fixture("write_done_slot2"),
    );
    assert!(matches!(
        feed(&mut playback, &wrong).event,
        Event::WrongWriteDone { slot: 9, .. }
    ));
    // Another session's.
    let other = from_dsp_to(
        apr::SVC_ASM,
        0x0201,
        2,
        0x0001_0D99,
        &fixture("write_done_slot2"),
    );
    assert_eq!(feed(&mut playback, &other).event, Event::None);
}

#[test]
fn nothing_is_written_or_run_before_everything_is_set_up() {
    let mut playback = Playback::new(config());
    assert_eq!(playback.write(3840), None);
    assert_eq!(playback.run(), None);
    let command = playback.start();
    assert_eq!(playback.write(3840), None);
    assert_eq!(playback.run(), None);
    let _ = feed(&mut playback, &answer(&command, 0));
    assert_eq!(playback.write(3840), None);
}

#[test]
fn each_service_has_linuxs_time_limit() {
    let mut playback = Playback::new(config());
    let mut command = playback.start();
    for limit in [3000, 3000, 3000, 5000, 5000, 1000, 1000, 5000] {
        assert_eq!(playback.answer_timeout_ms(), limit);
        if let Some(next) = feed(&mut playback, &answer(&command, 0)).send {
            command = next;
        }
    }
}

fn prepared() -> Playback {
    let mut playback = Playback::new(config());
    let mut command = playback.start();
    loop {
        match feed(&mut playback, &answer(&command, 0)).send {
            Some(next) => command = next,
            None => return playback,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_ready_question_is_linuxs_and_its_answers_are_read() {
    assert_eq!(hex(&avcs::get_state()), hex(&fixture("avcs_get_state")));
    assert_eq!(
        hex(&avcs::get_fwk_version()),
        hex(&fixture("avcs_fwk_version"))
    );
    let ready = from_dsp(apr::SVC_CORE, 0, avcs::RSP_GET_STATE, &1u32.to_le_bytes());
    assert_eq!(avcs::on_packet(&apr::parse(&ready).unwrap()), Some(true));
    let not_yet = from_dsp(apr::SVC_CORE, 0, avcs::RSP_GET_STATE, &0u32.to_le_bytes());
    assert_eq!(avcs::on_packet(&apr::parse(&not_yet).unwrap()), Some(false));
    let unsupported = basic(apr::SVC_CORE, avcs::CMD_GET_STATE, 3);
    assert_eq!(
        avcs::on_packet(&apr::parse(&unsupported).unwrap()),
        Some(true)
    );
    let other = basic(apr::SVC_AFE, avcs::CMD_GET_STATE, 0);
    assert_eq!(avcs::on_packet(&apr::parse(&other).unwrap()), None);
}

#[test]
fn a_refusal_while_starting_stops_there_and_only_what_ran_is_undone() {
    let mut playback = Playback::new(config());
    let mut command = playback.start();
    // Up to the port start, which the DSP refuses.
    for _ in 0..2 {
        command = feed(&mut playback, &answer(&command, 0)).send.unwrap();
    }
    let outcome = feed(&mut playback, &answer(&command, 2));
    assert_eq!(outcome.send, None);
    assert!(matches!(outcome.event, Event::Failed { status: 2, .. }));
    // Only the clock went on. The port never started, nothing was opened or mapped.
    let down = playback.stop();
    assert_eq!(down.len(), 1);
    assert_eq!(hex(&down[0]), hex(&fixture("clock_off")));
    let outcome = feed(&mut playback, &answer(&down[0], 0));
    assert_eq!(outcome.event, Event::Stopped);
}

#[test]
fn a_refused_step_on_the_way_down_does_not_stop_the_way_down() {
    let mut playback = prepared();
    let mut command = playback.stop().pop().unwrap();
    let mut seen = 0;
    loop {
        seen += 1;
        let outcome = feed(&mut playback, &answer(&command, 1));
        match outcome.send {
            Some(next) => command = next,
            None => {
                assert_eq!(outcome.event, Event::Stopped);
                break;
            }
        }
    }
    assert_eq!(seen, 5);
}

#[test]
fn the_way_down_sends_eos_only_from_a_stream_that_was_told_to_run() {
    let mut playback = prepared();
    assert_eq!(playback.stop().len(), 1);
    let mut playback = prepared();
    let run = playback.run().unwrap();
    assert_eq!(playback.stop().len(), 2);
    let _ = run;
}

#[test]
fn nonsense_changes_nothing() {
    let mut playback = Playback::new(config());
    let command = playback.start();
    for junk in [
        vec![],
        vec![0u8; 3],
        vec![0xff; 64],
        // The right answer from the wrong service, and for the wrong port.
        basic_to(apr::SVC_ASM, 0, 20, 0x0001_00F3, 0),
        basic_to(apr::SVC_AFE, 0, 21, 0x0001_00F3, 0),
        // An answer to another command.
        basic(apr::SVC_AFE, 0x0001_00E5, 0),
    ] {
        if let Some(packet) = apr::parse(&junk) {
            let outcome = playback.on_packet(&packet);
            assert_eq!(outcome.send, None);
            assert_eq!(outcome.event, Event::None);
        }
        assert_eq!(playback.step(), Step::ClockOn);
    }
    assert!(feed(&mut playback, &answer(&command, 0)).send.is_some());
}

#[test]
fn a_silent_dsp_ends_the_stream_without_more_commands() {
    let mut playback = Playback::new(config());
    let _ = playback.start();
    playback.on_timeout();
    assert_eq!(playback.step(), Step::Stopped);
    assert!(!playback.is_waiting());
    assert_eq!(playback.write(3840), None);
}
