//! Writes a 1 MiB image of an MMU-500 register window as Qualcomm firmware might leave it on SC7180, for the
//! QEMU test `tests/parity/qemu-smmu.toml`: QEMU has no MMU-500, so `scripts/qemu-smmu.sh` loads this image into
//! a `no-map` RAM region that a device tree node presents as the SMMU. Plain RAM: no hypervisor quirks, no side
//! effects; the quirks are covered by the crate's host tests.
//!
//! usage: cargo run -p lucina-mmu500 --example model-image -- <out.bin>
//!
//! Contents: 128 SMRs, 64 context banks, 4 KiB pages (context bank 0 at +0x80000); every S2CR FAULT; SMR 3 =
//! display (0x800, mask 0x2) bypass; SMR 4 = UFS (0xa0) translating in context bank 9, which is programmed; context
//! bank 63 translating for someone else.

use std::{env, fs, process::ExitCode};

const WINDOW: usize = 1 << 20;
const PAGE: usize = 1 << 12;

fn main() -> ExitCode {
    let Some(out) = env::args().nth(1) else {
        eprintln!("usage: model-image <out.bin>");
        return ExitCode::from(2);
    };
    let mut regs = vec![0u32; WINDOW / 4];
    let mut set = |offset: usize, value: u32| regs[offset / 4] = value;
    // ID0: S1TS, S2TS, NTS, SMS, CTTW, 15 stream ID bits, 128 SMRs. ID1: 128 global pages, 64 banks.
    set(
        0x20,
        (1 << 30) | (1 << 29) | (1 << 28) | (1 << 27) | (1 << 14) | (15 << 9) | 128,
    );
    set(0x24, (6 << 28) | 64);
    set(0x28, (1 << 12) | (5 << 8) | (5 << 4) | 5);
    set(0x3c, 0x21);
    for i in 0..128 {
        set(0xc00 + 4 * i, 2 << 16);
    }
    set(0x800 + 4 * 3, (1 << 31) | (0x2 << 16) | 0x800);
    set(0xc00 + 4 * 3, 1 << 16);
    set(0x800 + 4 * 4, (1 << 31) | 0xa0);
    set(0xc00 + 4 * 4, 9);
    set(PAGE + 4 * 9, 0x0001_ff00);
    set((128 + 9) * PAGE, 1);
    set((128 + 63) * PAGE, 1);
    let bytes: Vec<u8> = regs.iter().flat_map(|r| r.to_le_bytes()).collect();
    match fs::write(&out, bytes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("model-image: {out}: {e}");
            ExitCode::FAILURE
        }
    }
}
