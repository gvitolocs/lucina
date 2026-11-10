//! The sound device from a shell: `lucina-audio status | tone | play <file.wav> [dB]`.
//!
//! `tone` asks the kernel for its test tone, 1 kHz at -30 dB for two seconds; there is nothing to choose. `play` takes 16-bit stereo
//! WAV at 48 kHz and nothing else, at the given volume (the kernel starts a program at -30 dB and stops any request at -12 dB).

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

const CTL: &str = "/dev/lucina-audio-ctl";
const PCM: &str = "/dev/lucina-audio";

fn field(status: &str, name: &str) -> Option<String> {
    status
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
        .map(str::to_owned)
}

fn command(line: &str) -> Result<(), String> {
    let mut ctl = OpenOptions::new().write(true).open(CTL).map_err(|e| format!("{CTL}: {e}"))?;
    ctl.write_all(line.as_bytes()).map_err(|e| format!("{line}: {e}"))
}

fn wait_idle() -> Result<String, String> {
    loop {
        let status = fs::read_to_string(CTL).map_err(|e| format!("{CTL}: {e}"))?;
        match field(&status, "stream").as_deref() {
            Some("idle") | Some("none") | Some("broken") | Some("refused") => return Ok(status),
            _ => sleep(Duration::from_millis(50)),
        }
    }
}

/// The samples of a canonical 16-bit stereo 48 kHz WAV: the only format the speakers play.
fn wav_samples(file: &[u8]) -> Result<&[u8], String> {
    if file.len() < 12 || &file[..4] != b"RIFF" || &file[8..12] != b"WAVE" {
        return Err(String::from("not a WAV file"));
    }
    let mut at = 12;
    let mut format_ok = false;
    while at + 8 <= file.len() {
        let id = &file[at..at + 4];
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= file.len() {
            let f = &file[body..];
            let tag = u16::from_le_bytes([f[0], f[1]]);
            let channels = u16::from_le_bytes([f[2], f[3]]);
            let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            let bits = u16::from_le_bytes([f[14], f[15]]);
            if tag != 1 || channels != 2 || rate != 48_000 || bits != 16 {
                return Err(format!("{channels} channels, {rate} Hz, {bits} bits, format {tag}: only 16-bit stereo at 48 kHz"));
            }
            format_ok = true;
        }
        if id == b"data" {
            if !format_ok {
                return Err(String::from("samples before the format"));
            }
            return Ok(&file[body..(body + len).min(file.len())]);
        }
        at = body + len + (len & 1);
    }
    Err(String::from("no samples"))
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("status") | None => {
            print!("{}", fs::read_to_string(CTL).map_err(|e| format!("{CTL}: {e}"))?);
            Ok(())
        }
        Some("tone") => {
            command("tone")?;
            sleep(Duration::from_millis(200));
            print!("{}", wait_idle()?);
            Ok(())
        }
        Some("play") => {
            let path = args.get(1).ok_or("play <file.wav> [dB]")?;
            let file = fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            let samples = wav_samples(&file)?;
            let mut pcm = OpenOptions::new().write(true).open(PCM).map_err(|e| format!("{PCM}: {e}"))?;
            if let Some(db) = args.get(2) {
                command(&format!("volume {db}"))?;
            }
            pcm.write_all(samples).map_err(|e| format!("{PCM}: {e}"))?;
            drop(pcm);
            print!("{}", wait_idle()?);
            Ok(())
        }
        _ => Err(String::from("lucina-audio status | tone | play <file.wav> [dB]")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("lucina-audio: {err}");
            ExitCode::FAILURE
        }
    }
}
