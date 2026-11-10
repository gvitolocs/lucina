//! The microphone's VA macro and pins against Linux's own functions: `oracle/va` runs them for two captures and prints every access that
//! reaches the hardware (`tests/fixtures/va-oracle.txt`). The ops of `va.rs` run here on registers that start as Linux's oracle has
//! them (the VA macro at `va_defaults`, the pins at 0) and must print the same lines.

use lucina_q6::{
    rules::{self, CaptureStep, CodecPhase},
    va::{self, Block, Op, VaMacro},
};

struct Hardware {
    va: Vec<u32>,
    lpi: Vec<u32>,
    lines: Vec<String>,
}

impl Hardware {
    fn new() -> Hardware {
        // The ops read back only the DMIC clock registers, which are 0 in `va_defaults`, the oracle's reset state.
        let va = vec![0; (va::MAX_REGISTER / 4 + 1) as usize];
        Hardware {
            va,
            lpi: vec![0; 0x20000 / 4],
            lines: Vec::new(),
        }
    }

    fn regs(&mut self, block: Block) -> (&mut Vec<u32>, &'static str, usize) {
        match block {
            Block::Va => (&mut self.va, "va", 3),
            Block::Lpi => (&mut self.lpi, "lpi", 5),
        }
    }

    fn run(&mut self, ops: &[Op]) {
        for op in ops {
            assert!(
                op.allowed(),
                "{:?} is outside the VA macro and the DMIC pins",
                op
            );
            match *op {
                Op::Write {
                    block,
                    offset,
                    value,
                } => {
                    let (regs, name, w) = self.regs(block);
                    regs[offset as usize / 4] = value;
                    self.lines
                        .push(format!("{} w {:0w$x} {:08x}", name, offset, value));
                }
                Op::Update {
                    block,
                    offset,
                    mask,
                    value,
                }
                | Op::Replace {
                    block,
                    offset,
                    mask,
                    value,
                } => {
                    let (regs, name, w) = self.regs(block);
                    let orig = regs[offset as usize / 4];
                    let new = (orig & !mask) | (value & mask);
                    let mut lines = vec![format!("{} r {:0w$x} {:08x}", name, offset, orig)];
                    if new != orig || matches!(op, Op::Replace { .. }) {
                        regs[offset as usize / 4] = new;
                        lines.push(format!("{} w {:0w$x} {:08x}", name, offset, new));
                    }
                    self.lines.extend(lines);
                }
                Op::Delay { us } => self.lines.push(format!("delay {}", us)),
            }
        }
    }

    fn mark(&mut self, what: &str) {
        self.lines.push(format!("-- {}", what));
    }
}

fn oracle() -> Vec<String> {
    include_str!("fixtures/va-oracle.txt")
        .lines()
        // The clocks are the DSP's votes, held from the first capture on (`capture.rs`), not registers.
        .filter(|l| !l.starts_with("clk "))
        .map(String::from)
        .collect()
}

#[test]
fn two_captures_make_the_accesses_linux_makes() {
    let mic = va::mic_on_pair(0).unwrap();
    let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
    let mut hw = Hardware::new();
    for _ in 0..2 {
        hw.mark("open");
        hw.run(&va.open(mic));
        hw.mark("afe port config, port start");
        hw.run(&va.start());
        hw.mark("asm map, open read, adm open, matrix, encoder config, reads, run");
        hw.mark("eos");
        hw.run(&va.stop());
        hw.mark("afe port stop, asm close, unmap, adm close");
        va.close();
    }
    let want = oracle();
    for (i, (got, want)) in hw.lines.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "line {}", i + 1);
    }
    assert_eq!(hw.lines.len(), want.len());
}

#[test]
fn nothing_reaches_the_hardware_before_the_open() {
    let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
    assert!(va.start().is_empty());
    assert!(va.stop().is_empty());
}

#[test]
fn a_stop_without_a_start_writes_nothing() {
    let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
    let _ = va.open(va::mic_on_pair(1).unwrap());
    assert!(va.stop().is_empty());
}

#[test]
fn each_pair_muxes_its_own_pins_only() {
    for (pair, pins) in [
        (0, [0x6000, 0x7000]),
        (1, [0x8000, 0x9000]),
        (2, [0xC000, 0xD000]),
    ] {
        let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
        let ops = va.open(va::mic_on_pair(pair).unwrap());
        let lpi: Vec<u32> = ops
            .iter()
            .filter_map(|op| op.target())
            .filter(|(b, _)| *b == Block::Lpi)
            .map(|(_, o)| o)
            .collect();
        assert_eq!(lpi, pins);
        assert!(
            ops.iter()
                .chain(&va.start())
                .chain(&va.stop())
                .all(Op::allowed)
        );
    }
    assert_eq!(va::mic_on_pair(3), None);
}

#[test]
fn the_rule_refuses_everything_else() {
    assert!(rules::va_access_allowed(false, 0x7A8));
    assert!(!rules::va_access_allowed(false, 0x7AC));
    assert!(!rules::va_access_allowed(false, 0x402));
    assert!(!rules::va_access_allowed(true, 0x6008));
    assert!(!rules::va_access_allowed(true, 0xA000));
    assert!(!rules::va_access_allowed(true, 0x0000));
    assert!(
        !Op::Write {
            block: Block::Lpi,
            offset: 0x3000,
            value: 0
        }
        .allowed()
    );
}

#[test]
fn the_codec_goes_where_linux_puts_it() {
    assert_eq!(
        rules::codec_before(CaptureStep::PortConfig),
        CodecPhase::Open
    );
    assert_eq!(
        rules::codec_before(CaptureStep::MapMemory),
        CodecPhase::Start
    );
    assert_eq!(
        rules::capture_after_start(CaptureStep::PortStart),
        CaptureStep::MapMemory
    );
    assert_eq!(
        rules::codec_before(CaptureStep::PortStart),
        CodecPhase::Nothing
    );
}

#[test]
fn lane_masks() {
    assert_eq!(va::INTERNAL_MIC.codec_dma_lane_mask(), 0x4);
    assert_eq!(va::mic_on_pair(0).unwrap().codec_dma_lane_mask(), 0x1);
}

/// One capture as Linux's boards set it up: `qcom,dmic-sample-rate = <600000>` and kodiak's DMIC pin states
/// (`tests/fixtures/va-oracle-linux-boards.txt`, from `oracle/va` with `linux-boards`).
#[test]
fn linux_boards_settings_make_the_accesses_linux_makes() {
    let div = va::dmic_clk_div_for(va::LINUX_BOARDS_DMIC_HZ).unwrap();
    assert_eq!(div, 5);
    let mic = va::mic_on_pair(0).unwrap().with_pins(va::PinStates::LinuxBoards);
    let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
    va.set_dmic_clk_div(div);
    let mut hw = Hardware::new();
    hw.mark("open");
    hw.run(&va.open(mic));
    hw.mark("afe port config, port start");
    hw.run(&va.start());
    hw.mark("asm map, open read, adm open, matrix, encoder config, reads, run");
    hw.mark("eos");
    hw.run(&va.stop());
    hw.mark("afe port stop, asm close, unmap, adm close");
    va.close();
    let want: Vec<String> = include_str!("fixtures/va-oracle-linux-boards.txt")
        .lines()
        .filter(|l| !l.starts_with("clk "))
        .map(String::from)
        .collect();
    for (i, (got, want)) in hw.lines.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "line {}", i + 1);
    }
    assert_eq!(hw.lines.len(), want.len());
}

#[test]
fn the_divider_takes_only_what_the_macro_can_make() {
    assert_eq!(va::dmic_clk_div_for(4_800_000), Some(0));
    assert_eq!(va::dmic_clk_div_for(2_400_000), Some(2));
    assert_eq!(va::dmic_clk_div_for(1_000_000), None);
    assert_eq!(va::dmic_clk_div_for(0), None);
    assert_eq!(va::dmic_clk_div_for(9_600_000), None);
}

/// This laptop's internal mic as Windows sets it up (`va::INTERNAL_MIC`), through Linux's functions with those values
/// (`tests/fixtures/va-oracle-laptop.txt`, `oracle/va` with `laptop`).
#[test]
fn this_laptops_mic_makes_the_accesses_linux_makes_for_it() {
    let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
    let mut hw = Hardware::new();
    hw.mark("open");
    let open = va.open(va::INTERNAL_MIC);
    assert!(open.contains(&Op::Write { block: Block::Lpi, offset: 0xC004, value: 2 }));
    hw.run(&open);
    hw.mark("afe port config, port start");
    hw.run(&va.start());
    hw.mark("asm map, open read, adm open, matrix, encoder config, reads, run");
    hw.mark("eos");
    hw.run(&va.stop());
    hw.mark("afe port stop, asm close, unmap, adm close");
    va.close();
    let want: Vec<String> = include_str!("fixtures/va-oracle-laptop.txt")
        .lines()
        .filter(|l| !l.starts_with("clk "))
        .map(String::from)
        .collect();
    for (i, (got, want)) in hw.lines.iter().zip(&want).enumerate() {
        assert_eq!(got, want, "line {}", i + 1);
    }
    assert_eq!(hw.lines.len(), want.len());
}

#[test]
fn value_registers_only_of_the_dmic_pins() {
    assert!(rules::va_access_allowed(true, 0xC004));
    assert!(rules::va_access_allowed(true, 0x6004));
    assert!(!rules::va_access_allowed(true, 0xC008));
    assert!(!rules::va_access_allowed(true, 0xA004));
    assert!(!rules::va_access_allowed(true, 0xE000));
}

#[test]
fn the_gain_is_linuxs_volume_control_on_the_mics_decimator() {
    use lucina_q6::rules::{MIC_GAIN_MAX_DB, mic_gain_reg};
    // 0 dB is the reset value: the accesses stay Linux's (the other tests), and 30 dB reaches TX2_TX_VOL_CTL (0x50C) as 0x1E.
    let mut va = VaMacro::new(va::DMIC_CLK_DIV_DEFAULT);
    va.set_gain_db(30);
    let mut ops = va.open(va::INTERNAL_MIC);
    ops.extend(va.start());
    let wrote = |value: u32| ops.iter().any(|op| matches!(op, Op::Write { block: Block::Va, offset: 0x50C, value: v } if *v == value));
    assert!(wrote(0x1E), "{ops:?}");
    assert_eq!(mic_gain_reg(-3), 0xFD);
    assert_eq!(mic_gain_reg(99), MIC_GAIN_MAX_DB as u32);
    va.set_gain_db(-200);
    assert_eq!(va.gain_db(), -84);
}
