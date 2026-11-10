//! Writes a marker record to stdout, for `scripts/qemu-rng-marker.sh`: `rng_record clear|calling|skip <seq>`.
use std::io::Write;

use lucina_boot::rng_marker::{Record, State};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let state = match args[1].as_str() {
        "clear" => State::Clear,
        "calling" => State::Calling,
        "skip" => State::Skip,
        other => panic!("{other}?"),
    };
    let record = Record { seq: args[2].parse().unwrap(), state };
    std::io::stdout().write_all(&record.encode()).unwrap();
}
