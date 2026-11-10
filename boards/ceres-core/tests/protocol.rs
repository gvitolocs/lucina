// Integration tests for the Ceres Core protocol

use ceres_core::{
    CYCLE_DEFAULT, Command, Error, Last, Level, Line, LineReader, MAX_LINE, Plan, Reply, Status,
    Step, parse,
};

#[test]
fn every_command_parses() {
    assert_eq!(parse(b"help").unwrap(), Some(Command::Help));
    assert_eq!(parse(b"ver").unwrap(), Some(Command::Ver));
    assert_eq!(parse(b"status").unwrap(), Some(Command::Status));
    assert_eq!(
        parse(b"press 100").unwrap(),
        Some(Command::Press { ms: 100 })
    );
    assert_eq!(parse(b"cycle").unwrap(), Some(CYCLE_DEFAULT));
    assert_eq!(
        parse(b"cycle 3000 500 100").unwrap(),
        Some(Command::Cycle {
            off_ms: 3000,
            gap_ms: 500,
            on_ms: 100,
        })
    );
    assert_eq!(parse(b"release").unwrap(), Some(Command::Release));
    assert_eq!(parse(b"reboot").unwrap(), Some(Command::Reboot));
}

#[test]
fn case_insensitive() {
    assert_eq!(parse(b"HELP").unwrap(), Some(Command::Help));
    assert_eq!(parse(b"Press 10").unwrap(), Some(Command::Press { ms: 10 }));
    assert_eq!(parse(b"CyClE").unwrap(), Some(CYCLE_DEFAULT));
    assert_eq!(parse(b"ReLeAsE").unwrap(), Some(Command::Release));
}

#[test]
fn extra_spaces_tabs_and_trailing_cr() {
    assert_eq!(parse(b"  help  ").unwrap(), Some(Command::Help));
    assert_eq!(parse(b"\tver\t").unwrap(), Some(Command::Ver));
    assert_eq!(
        parse(b"press\t100").unwrap(),
        Some(Command::Press { ms: 100 })
    );
    assert_eq!(parse(b"status\r").unwrap(), Some(Command::Status));
}

#[test]
fn press_limits() {
    assert_eq!(parse(b"press 0").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press 1").unwrap(), Some(Command::Press { ms: 1 }));
    assert_eq!(
        parse(b"press 15000").unwrap(),
        Some(Command::Press { ms: 15000 })
    );
    assert_eq!(parse(b"press 15001").unwrap_err(), Error::BadArgument);
}

#[test]
fn cycle_off_limits() {
    assert_eq!(
        parse(b"cycle 2999 2000 300").unwrap_err(),
        Error::BadArgument
    );
    assert_eq!(
        parse(b"cycle 3000 2000 300").unwrap(),
        Some(Command::Cycle {
            off_ms: 3000,
            gap_ms: 2000,
            on_ms: 300,
        })
    );
    assert_eq!(
        parse(b"cycle 15000 2000 300").unwrap(),
        Some(Command::Cycle {
            off_ms: 15000,
            gap_ms: 2000,
            on_ms: 300,
        })
    );
    assert_eq!(
        parse(b"cycle 15001 2000 300").unwrap_err(),
        Error::BadArgument
    );
}

#[test]
fn cycle_gap_limits() {
    assert_eq!(
        parse(b"cycle 3000 499 300").unwrap_err(),
        Error::BadArgument
    );
    assert_eq!(
        parse(b"cycle 3000 500 300").unwrap(),
        Some(Command::Cycle {
            off_ms: 3000,
            gap_ms: 500,
            on_ms: 300,
        })
    );
    assert_eq!(
        parse(b"cycle 3000 10000 300").unwrap(),
        Some(Command::Cycle {
            off_ms: 3000,
            gap_ms: 10000,
            on_ms: 300,
        })
    );
    assert_eq!(
        parse(b"cycle 3000 10001 300").unwrap_err(),
        Error::BadArgument
    );
}

#[test]
fn cycle_on_limits() {
    assert_eq!(
        parse(b"cycle 3000 2000 99").unwrap_err(),
        Error::BadArgument
    );
    assert_eq!(
        parse(b"cycle 3000 2000 100").unwrap(),
        Some(Command::Cycle {
            off_ms: 3000,
            gap_ms: 2000,
            on_ms: 100,
        })
    );
    assert_eq!(
        parse(b"cycle 3000 2000 2000").unwrap(),
        Some(Command::Cycle {
            off_ms: 3000,
            gap_ms: 2000,
            on_ms: 2000,
        })
    );
    assert_eq!(
        parse(b"cycle 3000 2000 2001").unwrap_err(),
        Error::BadArgument
    );
}

#[test]
fn cycle_defaults_are_6000_2000_300() {
    assert_eq!(
        CYCLE_DEFAULT,
        Command::Cycle {
            off_ms: 6000,
            gap_ms: 2000,
            on_ms: 300,
        }
    );
    assert_eq!(parse(b"cycle").unwrap(), Some(CYCLE_DEFAULT));
}

#[test]
fn cycle_wrong_argument_count() {
    assert_eq!(parse(b"cycle 1").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"cycle 1 2").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"cycle 1 2 3 4").unwrap_err(), Error::BadArgument);
}

#[test]
fn bad_numbers() {
    assert_eq!(parse(b"press -5").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press +5").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press 5ms").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press 1.5").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press 0x10").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press 4294967296").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press 99999999999").unwrap_err(), Error::BadArgument);
    assert_eq!(
        parse(b"press 0300").unwrap(),
        Some(Command::Press { ms: 300 })
    );
}

#[test]
fn extra_arguments() {
    assert_eq!(parse(b"help extra").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"ver extra").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"status extra").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"release extra").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"reboot extra").unwrap_err(), Error::BadArgument);
    assert_eq!(parse(b"press").unwrap_err(), Error::BadArgument);
}

#[test]
fn unknown_commands() {
    assert_eq!(parse(b"foo").unwrap_err(), Error::UnknownCommand);
    assert_eq!(parse(b"pres").unwrap_err(), Error::UnknownCommand);
    assert_eq!(parse(b"presss 10").unwrap_err(), Error::UnknownCommand);
    assert_eq!(parse(&[0xff, 0xfe]).unwrap_err(), Error::UnknownCommand);
}

#[test]
fn blank_lines_get_no_reply() {
    assert_eq!(parse(b"").unwrap(), None);
    assert_eq!(parse(b"   ").unwrap(), None);
    assert_eq!(parse(b"\t").unwrap(), None);
    assert_eq!(parse(b"\r").unwrap(), None);
}

#[test]
fn reply_ready_help_ver() {
    assert_eq!(
        Reply::Ready.to_string(),
        "ceres ready v0.1 board=esp32 role=dsi-power pin=25"
    );
    assert_eq!(
        Reply::Help.to_string(),
        "ok commands: help ver status press <ms> cycle [off_ms gap_ms on_ms] release reboot"
    );
    assert_eq!(
        Reply::Ver.to_string(),
        "ok ceres v0.1 board=esp32 role=dsi-power pin=25"
    );
}

#[test]
fn reply_status_both_button_states_and_each_last() {
    let base = Status {
        up_ms: 1000,
        pressed: false,
        presses: 5,
        last: Last::None,
    };
    assert_eq!(
        Reply::Status(base).to_string(),
        "ok up_ms=1000 button=released presses=5 last=none"
    );
    assert_eq!(
        Reply::Status(Status {
            pressed: true,
            ..base
        })
        .to_string(),
        "ok up_ms=1000 button=pressed presses=5 last=none"
    );
    for (last, name) in [
        (Last::None, "none"),
        (Last::Press, "press"),
        (Last::Cycle, "cycle"),
        (Last::Release, "release"),
    ] {
        assert_eq!(
            Reply::Status(Status { last, ..base }).to_string(),
            format!("ok up_ms=1000 button=released presses=5 last={name}")
        );
    }
}

#[test]
fn reply_pressed_cycled_released_rebooting() {
    assert_eq!(Reply::Pressed { ms: 100 }.to_string(), "ok pressed 100");
    assert_eq!(
        Reply::Cycled {
            off_ms: 6000,
            gap_ms: 2000,
            on_ms: 300,
        }
        .to_string(),
        "ok cycled off=6000 gap=2000 on=300"
    );
    assert_eq!(Reply::Released.to_string(), "ok released");
    assert_eq!(Reply::Rebooting.to_string(), "ok rebooting");
}

#[test]
fn reply_err_reasons() {
    for (err, reason) in [
        (Error::UnknownCommand, "unknown command"),
        (Error::BadArgument, "bad argument"),
        (Error::Busy, "busy"),
        (Error::Released, "released"),
        (Error::LineTooLong, "line too long"),
    ] {
        assert_eq!(err.reason(), reason);
        assert_eq!(Reply::Err(err).to_string(), format!("err {reason}"));
    }
}

/// Feeds bytes one at a time; returns the line at each `\n`.
fn feed(reader: &mut LineReader, bytes: &[u8]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for &b in bytes {
        if let Some(line) = reader.push(b) {
            match line {
                Line::Text(t) => out.push(Line::Text(&t.to_vec().leak()[..])),
                Line::TooLong => out.push(Line::TooLong),
            }
        }
    }
    out
}

#[test]
fn line_reader_endings_and_multiple_lines() {
    let mut reader = LineReader::new();
    assert_eq!(feed(&mut reader, b"hello\n"), vec![Line::Text(b"hello")]);
    assert_eq!(feed(&mut reader, b"world\r\n"), vec![Line::Text(b"world")]);
    assert_eq!(
        feed(&mut reader, b"multi\nline\n"),
        vec![Line::Text(b"multi"), Line::Text(b"line")]
    );
}

#[test]
fn line_reader_no_line_before_newline() {
    let mut reader = LineReader::new();
    assert!(feed(&mut reader, b"partial").is_empty());
    assert_eq!(
        feed(&mut reader, b" rest\n"),
        vec![Line::Text(b"partial rest")]
    );
}

#[test]
fn line_reader_max_line_boundaries() {
    let mut reader = LineReader::new();
    let line80 = [b'a'; MAX_LINE];
    assert!(feed(&mut reader, &line80).is_empty());
    assert_eq!(feed(&mut reader, b"\n"), vec![Line::Text(&line80)]);

    let mut reader = LineReader::new();
    assert!(feed(&mut reader, &line80).is_empty());
    assert_eq!(feed(&mut reader, b"\r\n"), vec![Line::Text(&line80)]);

    let mut reader = LineReader::new();
    let line81 = [b'a'; MAX_LINE + 1];
    assert!(feed(&mut reader, &line81).is_empty());
    assert_eq!(feed(&mut reader, b"\n"), vec![Line::TooLong]);
}

#[test]
fn line_reader_too_long_recovers() {
    let mut reader = LineReader::new();
    let long200 = [b'a'; 200];
    assert!(feed(&mut reader, &long200).is_empty());
    assert_eq!(feed(&mut reader, b"\n"), vec![Line::TooLong]);
    assert_eq!(feed(&mut reader, b"ok\n"), vec![Line::Text(b"ok")]);
}

#[test]
fn line_reader_lone_cr_is_kept() {
    let mut reader = LineReader::new();
    assert_eq!(
        feed(&mut reader, b"mid\rline\n"),
        vec![Line::Text(b"mid\rline")]
    );
}

#[test]
fn plan_of_press() {
    let plan = Plan::of(Command::Press { ms: 100 }).unwrap();
    assert_eq!(
        plan.steps(),
        &[Step {
            level: Level::High,
            ms: 100,
        }]
    );
    assert_eq!(plan.total_ms(), 100);
    assert_eq!(plan.done, Reply::Pressed { ms: 100 });
    assert_eq!(plan.last, Last::Press);
}

#[test]
fn plan_of_cycle() {
    let plan = Plan::of(CYCLE_DEFAULT).unwrap();
    assert_eq!(
        plan.steps(),
        &[
            Step {
                level: Level::High,
                ms: 6000,
            },
            Step {
                level: Level::Low,
                ms: 2000,
            },
            Step {
                level: Level::High,
                ms: 300,
            },
        ]
    );
    assert_eq!(plan.total_ms(), 8300);
    assert_eq!(
        plan.done,
        Reply::Cycled {
            off_ms: 6000,
            gap_ms: 2000,
            on_ms: 300,
        }
    );
    assert_eq!(plan.last, Last::Cycle);
}

#[test]
fn plan_of_other_commands_is_none() {
    assert_eq!(Plan::of(Command::Help), None);
    assert_eq!(Plan::of(Command::Ver), None);
    assert_eq!(Plan::of(Command::Status), None);
    assert_eq!(Plan::of(Command::Release), None);
    assert_eq!(Plan::of(Command::Reboot), None);
}
