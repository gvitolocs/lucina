//! Flips bits in part of a file, for testing what a reset that loses bits does to the saved log: `scripts/qemu-crash.sh` runs it on the
//! guest's RAM between a crash and QEMU's reset. The sequence is fixed, so that a failure repeats.
//!
//! usage: flip-bits <file> <offset, hexadecimal> <length, hexadecimal> <one bit in how many>

use std::{fs::OpenOptions, os::unix::fs::FileExt};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, offset, length, one_in] = args.as_slice() else {
        panic!("usage: flip-bits <file> <offset> <length> <one bit in how many>");
    };
    let hex =
        |text: &str| u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("hexadecimal");
    let (offset, length) = (hex(offset), hex(length) as usize);
    let one_in: u64 = one_in.parse().expect("a number");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open");
    let mut data = vec![0u8; length];
    file.read_exact_at(&mut data, offset).expect("read");
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut flipped = 0u64;
    for byte in data.iter_mut() {
        for bit in 0..8 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if x.is_multiple_of(one_in) {
                *byte ^= 1 << bit;
                flipped += 1;
            }
        }
    }
    file.write_all_at(&data, offset).expect("write");
    println!("flip-bits: {flipped} bits of {} flipped", length * 8);
}
