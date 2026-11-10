//! Writes the RAM image of `scripts/qemu-rtc.sh`, a stand-in for the Galaxy Book Go's SPMI arbiter: the arbiter's version and channel map at
//! 0, and at 64 KiB its observer channels, with channel 2 always done and holding a counter of 14464. Its low byte has bit 7 set, so that the
//! one data register also reads as a clock control register that says "enabled". RAM runs no commands; the protocol is tested on a model in
//! this crate's tests.
//!
//! usage: model-image <file>

fn put(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: model-image <file>");
    let mut image = vec![0u8; 0x20000];
    put(&mut image, 0x0, 0x5000_0000);
    put(&mut image, 0x4, 8);
    // The map: channel 2 is SID 0, peripheral 0x60.
    put(&mut image, 0x900 + 4 * 2, 0x060 << 8);
    let channel = 0x10000 + 0x80 * 2;
    put(&mut image, channel + 0x08, 1);
    put(&mut image, channel + 0x18, 14464);
    std::fs::write(path, image).expect("write the image");
}
