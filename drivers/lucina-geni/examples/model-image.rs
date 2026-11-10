//! Writes a 1 MiB image for the QEMU smoke test `tests/parity/qemu-hid.toml` (`scripts/qemu-hid.sh`): a GENI
//! serial engine's registers at +0, a second engine at +0x4000 (never to be touched: its clock is off), its QUP
//! wrapper's at +0x8000, a GCC at +0x10000 (the clocks read as running except serial engine 4's, serial engine 0's root
//! with DFS on and a table whose level 2 is 19.2 MHz) and a TLMM tile at +0x80000 (the keyboard's interrupt pad not set,
//! its line high), loaded into a `no-map` RAM region that device tree nodes present as `qcom,geni-se-qup` /
//! `qcom,geni-i2c` with a `hid-over-i2c` keyboard. QEMU has no GENI.
//!
//! RAM does not execute commands, so the image holds what a running engine would show: I2C firmware, the FIFO
//! enabled, 16-word FIFOs, the firmware's 400 kHz timing, `M_IRQ_STATUS` = CMD_DONE | TX watermark (every command
//! "finishes" at once), and an RX FIFO that always holds 8 words of `1e 00 00 01`. The kernel then reads a valid HID
//! descriptor made of that word repeated (length 30, version 1.00, VID 001e, PID 0100), sees no reset acknowledgement
//! and no keyboard in the report descriptor, and reports each step: the real MMIO, thread and error paths, not the
//! protocol, which the crates' host tests cover.
//!
//! usage: cargo run -p lucina-geni --example model-image -- <out.bin>

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(out) = env::args().nth(1) else {
        eprintln!("usage: model-image <out.bin>");
        return ExitCode::from(2);
    };
    let mut regs = vec![0u32; 0x100000 / 4];
    let mut set = |offset: usize, value: u32| regs[offset / 4] = value;
    set(0x68, (3 << 8) | 0x41); // GENI_FW_REVISION_RO: I2C
    set(0x48, 0x21); // GENI_SER_M_CLK_CFG
    set(0x7c, 2); // SE_GENI_CLK_SEL
    set(0x278, 0x0050_3018); // SE_I2C_SCL_COUNTERS: 400 kHz at 19.2 MHz
    set(0xe24, (32 << 24) | (16 << 16)); // SE_HW_PARAM_0
    set(0xe28, (32 << 24) | (16 << 16)); // SE_HW_PARAM_1
    set(0x610, (1 << 30) | 1); // SE_GENI_M_IRQ_STATUS: TX watermark, CMD_DONE
    set(0x804, 8); // SE_GENI_RX_FIFO_STATUS
    set(0x780, 0x0100_001e); // SE_GENI_RX_FIFOn
    set(0x8004, 0x2000_0000); // QUP_HW_VER_REG of the wrapper: 2.0
    set(0x10000 + 0x184d4, 1 << 31); // GCC_QUPV3_WRAP1_S4_CBCR: CLK_OFF; RAM ignores the vote
    set(0x10000 + 0x18010, (1 << 31) | 2); // GCC_QUPV3_WRAP_1_S_AHB_CBCR: CLK_OFF, in hardware gated mode
    // gcc_qupv3_wrap1_s0_clk_src: DFS on at level 2, of 7.3728, 14.7456 and 19.2 MHz from GPLL0 even and TCXO.
    let s0 = 0x10000 + 0x18018;
    set(s0 + 0x14, 1 | (2 << 1));
    for (level, (cfg, m, n)) in [
        ((6 << 8) | (2 << 12) | 1, 384u32, 15625u32),
        ((6 << 8) | (2 << 12) | 1, 768, 15625),
        (1, 0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        set(s0 + 0x1c + 4 * level, cfg);
        set(s0 + 0x5c + 4 * level, m);
        set(s0 + 0x9c + 4 * level, !(n.wrapping_sub(m)));
    }
    set(0x80000 + 0x68004, 1); // TLMM GPIO 104's io register: the line high
    let bytes: Vec<u8> = regs.iter().flat_map(|r| r.to_le_bytes()).collect();
    match fs::write(&out, bytes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("model-image: {out}: {e}");
            ExitCode::FAILURE
        }
    }
}
