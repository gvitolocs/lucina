//! A whole capture from the internal microphone against a model of the DSP, with every packet compared to what Linux's own q6afe.c,
//! q6adm.c and q6asm.c sent for the same stream (`oracle/`).

use lucina_q6::apr::{self, BASIC_RSP_RESULT, DOMAIN_ADSP, HDR_SIZE, SEQ_CMD_HDR_FIELD};
use lucina_q6::capture::{Capture, CaptureStep, Config, Event, INTERNAL_MIC};
use lucina_q6::va;

const HANDLE: u32 = 0xabcd_1234;
const COPP: u16 = 0x11;

fn config() -> Config {
    Config {
        phys: 0x1_2346_6000,
        period_bytes: 1920,
        periods: 8,
        ..INTERNAL_MIC
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_dsp(svc: u8, dest_port: u16, token: u32, opcode: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&SEQ_CMD_HDR_FIELD.to_le_bytes());
    out.extend_from_slice(&((HDR_SIZE + payload.len()) as u16).to_le_bytes());
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

/// What the model DSP answers: `q6afe_callback`, `q6adm_callback`, `q6asm_srvc_callback`.
fn answer(command: &[u8], status: u32) -> Vec<u8> {
    let word = |at: usize| u32::from_le_bytes(command[at..at + 4].try_into().unwrap());
    let (svc, src_port, token, opcode) = (
        command[4],
        u16::from_le_bytes([command[6], command[7]]),
        word(12),
        word(16),
    );
    match opcode {
        0x0001_0D92 if status == 0 => {
            from_dsp(svc, src_port, token, 0x0001_0D93, &HANDLE.to_le_bytes())
        }
        0x0001_00F4 => from_dsp(
            svc,
            src_port,
            token,
            0x0001_00F5,
            &[0u32.to_le_bytes(), status.to_le_bytes()].concat(),
        ),
        0x0001_0326 if status == 0 => {
            let mut p = 0u32.to_le_bytes().to_vec();
            p.extend_from_slice(&COPP.to_le_bytes());
            p.extend_from_slice(&0u16.to_le_bytes());
            from_dsp(svc, src_port, token, 0x0001_0329, &p)
        }
        _ => {
            let mut p = opcode.to_le_bytes().to_vec();
            p.extend_from_slice(&status.to_le_bytes());
            from_dsp(svc, src_port, token, BASIC_RSP_RESULT, &p)
        }
    }
}

fn feed(capture: &mut Capture, bytes: &[u8]) -> lucina_q6::capture::Outcome {
    capture.on_packet(&apr::parse(bytes).unwrap())
}

fn prepared() -> Capture {
    let mut capture = Capture::new(config());
    let mut command = capture.start();
    loop {
        match feed(&mut capture, &answer(&command, 0)).send {
            Some(next) => command = next,
            None => return capture,
        }
    }
}

#[test]
fn a_capture_says_what_linux_says_in_linuxs_order() {
    let mut capture = Capture::new(config());
    let mut command = capture.start();
    for name in [
        "mic_vote_macro",
        "mic_vote_dcodec",
        "mic_bus_clock",
        "mic_mclk",
        "mic_port_config",
        "mic_port_start",
        "mic_map_memory",
        "mic_open_read",
        "mic_adm_open",
        "mic_matrix",
        "mic_encoder_config",
    ] {
        assert_eq!(hex(&command), hex(&fixture(name)), "{name}");
        assert!(capture.is_waiting());
        let outcome = feed(&mut capture, &answer(&command, 0));
        match outcome.send {
            Some(next) => command = next,
            None => {
                assert_eq!(name, "mic_encoder_config");
                assert_eq!(outcome.event, Event::Prepared);
            }
        }
    }
    assert_eq!(capture.step(), CaptureStep::Prepared);
    for slot in 0..8 {
        assert_eq!(
            hex(&capture.read().unwrap()),
            hex(&fixture(&format!("mic_read_slot{slot}")))
        );
    }
    let run = capture.run().unwrap();
    assert_eq!(hex(&run), hex(&fixture("mic_run")));
    assert_eq!(feed(&mut capture, &answer(&run, 0)).event, Event::Running);

    let done = from_dsp(
        apr::SVC_ASM,
        0x0201,
        3,
        0x0001_0D9A,
        &fixture("mic_read_done_slot3"),
    );
    assert_eq!(feed(&mut capture, &done).event, Event::ReadDone { slot: 3 });
    assert_eq!(capture.slot_offset(3), Some(3 * 1920));

    let mut down = capture.stop();
    assert_eq!(hex(&down[0]), hex(&fixture("mic_eos")));
    let mut command = down.pop().unwrap();
    for name in ["mic_port_stop", "mic_close", "mic_unmap", "mic_adm_close"] {
        assert_eq!(hex(&command), hex(&fixture(name)), "{name}");
        let outcome = feed(&mut capture, &answer(&command, 0));
        match outcome.send {
            Some(next) => command = next,
            None => {
                assert_eq!(name, "mic_adm_close");
                assert_eq!(outcome.event, Event::Stopped);
            }
        }
    }
    assert_eq!(capture.read(), None);
}

#[test]
fn the_laptop_mic_starts_its_port_on_lane_2() {
    let mut capture = Capture::with_votes_held(Config {
        channel_mask: va::INTERNAL_MIC.codec_dma_lane_mask(),
        ..config()
    });
    let command = capture.start();
    assert_eq!(hex(&command), hex(&fixture("mic_port_config_lane2")));
    let outcome = feed(&mut capture, &answer(&command, 0));
    assert_eq!(hex(&outcome.send.as_ref().unwrap()), hex(&fixture("mic_port_start_lane2")));
}

#[test]
fn a_read_done_for_another_address_or_session_proves_nothing() {
    let mut capture = prepared();
    let wrong = from_dsp(apr::SVC_ASM, 0x0201, 3, 0x0001_0D9A, &[0; 12]);
    assert!(matches!(
        feed(&mut capture, &wrong).event,
        Event::WrongReadDone { slot: 3, .. }
    ));
    let playback = from_dsp(
        apr::SVC_ASM,
        0x0101,
        3,
        0x0001_0D9A,
        &fixture("mic_read_done_slot3"),
    );
    assert_eq!(feed(&mut capture, &playback).event, Event::None);
}

#[test]
fn nothing_is_read_or_run_before_everything_is_set_up() {
    let mut capture = Capture::new(config());
    assert_eq!(capture.read(), None);
    assert_eq!(capture.run(), None);
    let command = capture.start();
    assert_eq!(capture.read(), None);
    let _ = feed(&mut capture, &answer(&command, 0));
    assert_eq!(capture.run(), None);
}

#[test]
fn a_refusal_stops_there_and_only_what_ran_is_undone() {
    let mut capture = Capture::new(config());
    let command = capture.start();
    let mut next = command;
    for _ in 0..6 {
        next = feed(&mut capture, &answer(&next, 0)).send.unwrap();
    }
    // The DSP refuses the memory map: only the port started.
    assert!(matches!(
        feed(&mut capture, &answer(&next, 2)).event,
        Event::Failed { status: 2, .. }
    ));
    let down = capture.stop();
    assert_eq!(down.len(), 1);
    assert_eq!(hex(&down[0]), hex(&fixture("mic_port_stop")));
    assert_eq!(
        feed(&mut capture, &answer(&down[0], 0)).event,
        Event::Stopped
    );
}

#[test]
fn a_second_capture_does_not_vote_again() {
    let mut capture = Capture::with_votes_held(config());
    assert_eq!(hex(&capture.start()), hex(&fixture("mic_port_config")));
}

#[test]
fn a_refused_vote_stops_there() {
    let mut capture = Capture::new(config());
    let vote = capture.start();
    assert!(matches!(
        feed(&mut capture, &answer(&vote, 1)).event,
        Event::Failed { .. }
    ));
    assert!(!capture.votes_held());
    assert!(capture.stop().is_empty());
}

#[test]
fn the_laptop_mic_asks_for_0x30c_alone_as_windows_does() {
    // Windows votes clock node 10 alone for a VA DMIC recording (juno-audio.md section 28): Linux's sequence without the bus clock.
    let mut capture = Capture::new(config()).with_mclk_only();
    assert!(capture.mclk_only());
    let mut command = capture.start();
    for name in [
        "mic_vote_macro",
        "mic_vote_dcodec",
        "mic_mclk",
        "mic_port_config",
        "mic_port_start",
        "mic_map_memory",
        "mic_open_read",
        "mic_adm_open",
        "mic_matrix",
        "mic_encoder_config",
    ] {
        assert_eq!(hex(&command), hex(&fixture(name)), "{name}");
        let outcome = feed(&mut capture, &answer(&command, 0));
        match outcome.send {
            Some(next) => command = next,
            None => assert_eq!(name, "mic_encoder_config"),
        }
    }
    assert_eq!(capture.step(), CaptureStep::Prepared);
    assert!(capture.votes_held());
    // The clock it asked for is Windows' node 10.
    let id = lucina_q6::rules::windows_va_clock_id(false).to_le_bytes();
    assert!(fixture("mic_mclk").windows(4).any(|w| w == id));
    assert!(!fixture("mic_mclk").windows(4).any(|w| w == lucina_q6::rules::windows_va_clock_id(true).to_le_bytes()));
}
