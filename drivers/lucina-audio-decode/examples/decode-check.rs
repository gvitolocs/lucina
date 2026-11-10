//! `decode-check <file>...`: decodes each file to the end and says what came out and how fast. For judging the decoder against real
//! files, on the host or on the laptop.

use std::path::Path;
use std::time::Instant;

use lucina_audio_decode::Decoder;

fn main() {
    let mut failed = false;
    for arg in std::env::args().skip(1) {
        let started = Instant::now();
        let mut decoder = match Decoder::open(Path::new(&arg)) {
            Ok(decoder) => decoder,
            Err(err) => {
                println!("{arg}: {err}");
                failed = true;
                continue;
            }
        };
        let rate = decoder.rate();
        let (mut bytes, mut peak) = (0u64, 0i32);
        let mut block = Vec::new();
        let result = loop {
            block.clear();
            match decoder.next_block(&mut block) {
                Ok(more) => {
                    bytes += block.len() as u64;
                    for s in block.chunks_exact(2) {
                        peak = peak.max(i32::from(i16::from_le_bytes([s[0], s[1]])).abs());
                    }
                    if !more {
                        break Ok(());
                    }
                }
                Err(err) => break Err(err),
            }
        };
        let seconds = bytes as f64 / 192_000.0;
        let took = started.elapsed().as_secs_f64();
        println!(
            "{arg}: {rate} Hz in, {seconds:.2} s out, peak {peak}, {} packets skipped, decoded in {took:.3} s ({:.0}x real time){}",
            decoder.skipped_packets,
            seconds / took.max(1e-6),
            match result {
                Ok(()) => String::new(),
                Err(err) => {
                    failed = true;
                    format!(", then: {err}")
                }
            },
        );
    }
    std::process::exit(i32::from(failed));
}
