//! Writes a 512 KiB image for the QEMU smoke test `tests/parity/qemu-charge.toml` (`scripts/qemu-charge.sh`): a GENI
//! serial engine's registers at +0, its QUP wrapper's at +0x8000 and a GCC at +0x10000, loaded into a `no-map` RAM region
//! that device tree nodes present as the Galaxy Book Go's battery bus. QEMU has no GENI.
//!
//! RAM does not execute commands, so the image holds what a running engine would show: I2C firmware, the FIFO enabled,
//! 16-word FIFOs, the firmware's 400 kHz timing, `M_IRQ_STATUS` = CMD_DONE | TX watermark (every command "finishes" at
//! once), and an RX FIFO that always holds 8 words of `0x0032`. Every word read from either chip is then 50: the kernel
//! sees a chip at 0x09 that is not a BQ25710, and a gauge at 50 % that reads 50 mV. That is the real device tree, MMIO,
//! thread, refusal and sysfs path, not the protocol or the rules, which the crate's host tests cover.
//!
//! usage: cargo run -p lucina-charge --example model-image -- <out.bin>

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(out) = env::args().nth(1) else {
        eprintln!("usage: model-image <out.bin>");
        return ExitCode::from(2);
    };
    let mut regs = vec![0u32; 0x80000 / 4];
    let mut set = |offset: usize, value: u32| regs[offset / 4] = value;
    set(0x68, (3 << 8) | 0x41); // GENI_FW_REVISION_RO: I2C
    set(0x48, 0x21); // GENI_SER_M_CLK_CFG
    set(0x278, 0x0050_3018); // SE_I2C_SCL_COUNTERS: 400 kHz at 19.2 MHz
    set(0xe24, (32 << 24) | (16 << 16)); // SE_HW_PARAM_0
    set(0xe28, (32 << 24) | (16 << 16)); // SE_HW_PARAM_1
    set(0x610, (1 << 30) | 1); // SE_GENI_M_IRQ_STATUS: TX watermark, CMD_DONE
    set(0x804, 8); // SE_GENI_RX_FIFO_STATUS
    set(0x780, 0x0000_0032); // SE_GENI_RX_FIFOn
    set(0x8004, 0x2000_0000); // QUP_HW_VER_REG of the wrapper: 2.0
    // The GCC's clocks read as running: CLK_OFF (bit 31) clear everywhere.
    let bytes: Vec<u8> = regs.iter().flat_map(|r| r.to_le_bytes()).collect();
    match fs::write(&out, bytes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("model-image: {out}: {e}");
            ExitCode::FAILURE
        }
    }
}
