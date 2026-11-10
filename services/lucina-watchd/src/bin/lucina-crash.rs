//! lucina-crash: shows the crashes the kernel saved (/sys/fs/pstore), from a shell or over SSH.
//!
//! ```text
//! lucina-crash            the list, then the last boot's report if it ended badly
//! lucina-crash list       the list
//! lucina-crash show <n>   one report, by the number in the list
//! lucina-crash log        everything the last boot logged
//! lucina-crash notice     one line if the last boot ended badly, for the login
//! lucina-crash clear      forgets the saved reports
//! ```
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{fs, process::ExitCode};

use lucina_watchd::{Status, clean_text, notice};

const DIR: &str = "/sys/fs/pstore";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Ok(text) = fs::read_to_string(format!("{DIR}/status")) else {
        // At a login this must stay silent.
        if args.first() != Some(&"notice") {
            eprintln!("lucina-crash: no saved log: this boot has no RAM region for it");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    };
    let status = Status::parse(&text);
    match args.as_slice() {
        [] => {
            list(&status);
            if let Some(saved) = status.crash_of_last_boot() {
                println!();
                return show(saved.seq, &status);
            }
            ExitCode::SUCCESS
        }
        ["list"] => {
            list(&status);
            ExitCode::SUCCESS
        }
        ["show", seq] => match seq.parse() {
            Ok(seq) => show(seq, &status),
            Err(_) => usage(),
        },
        ["log"] => {
            match fs::read(format!("{DIR}/console-lucina-0")) {
                Ok(text) => print!("{}", clean_text(&text)),
                Err(err) => {
                    eprintln!("lucina-crash: {err}");
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        ["notice"] => {
            if let Some(saved) = status.crash_of_last_boot() {
                print!("{}", notice(&system_name(), saved));
            }
            ExitCode::SUCCESS
        }
        ["clear"] => match fs::write(format!("{DIR}/erase"), "all") {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("lucina-crash: {err}");
                ExitCode::FAILURE
            }
        },
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: lucina-crash [list | show <n> | log | notice | clear]");
    ExitCode::FAILURE
}

fn list(status: &Status) {
    println!(
        "boot {}, last boot {}, crashes in a row: {}",
        status.boot, status.last_boot, status.crashes_in_a_row
    );
    if status.saved.is_empty() {
        println!("no saved crashes");
    }
    for saved in &status.saved {
        println!(
            "{:>4}  boot {:<5} {:<18} {}",
            saved.seq, saved.boot, saved.kind, saved.reason
        );
    }
}

fn show(seq: u32, status: &Status) -> ExitCode {
    let Some(saved) = status.saved.iter().find(|saved| saved.seq == seq) else {
        eprintln!("lucina-crash: no saved crash {seq}");
        return ExitCode::FAILURE;
    };
    match fs::read(format!("{DIR}/dmesg-lucina-{}", saved.slot)) {
        Ok(text) => {
            print!("{}", clean_text(&text));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("lucina-crash: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `Lucina Juno` from the `5.13.0-LucinaJuno` in `/proc/version`, as on the crash screen.
fn system_name() -> String {
    fs::read_to_string("/proc/version")
        .ok()
        .and_then(|version| {
            let name = version
                .split_ascii_whitespace()
                .find_map(|word| word.rsplit_once("-Lucina"))?
                .1;
            (!name.is_empty() && name != "OS").then(|| format!("Lucina {name}"))
        })
        .unwrap_or_else(|| "LucinaOS".to_string())
}
