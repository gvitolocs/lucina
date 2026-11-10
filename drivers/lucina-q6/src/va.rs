//! The internal microphone's digital path on this side of the DSP: Linux's VA macro (`lpass-va-macro.c`, the sm8250 variant this
//! laptop's device tree names) and the LPI pin mux (`pinctrl-lpass-lpi.c`, the SM8250 table, the nearest Linux has to SC7180), as the
//! register accesses Linux makes for one capture. Nothing here touches hardware: each phase gives a list of [`Op`]s, which the kernel
//! runs only after [`rules::va_access_allowed`] says each one stays in the VA macro or on a DMIC pin.
//!
//! Linux's regmap stands between the driver and the hardware and decides what reaches it: a flat cache filled from `va_defaults`,
//! `update_bits` that writes only a changed value, a cache-only mode while runtime-suspended, and `regcache_sync`, which after
//! `regcache_mark_dirty` writes back every cached value that is not its default. [`Regmap`] is that code for an MMIO map, and the
//! phases are the functions DAPM and ALSA call, in their order (`docs/research/juno-audio.md` section 13, and the oracle in
//! `oracle/va`, which runs Linux's own functions and prints what reaches the hardware).
//!
//! | Phase | When, in Linux's DPCM order for a capture | Linux |
//! |---|---|---|
//! | [`VaMacro::new`] | probe, then runtime autosuspend | `va_macro_probe`, `va_macro_runtime_suspend` |
//! | [`VaMacro::open`] | BE open and hw_params, before the AFE port | the mixer controls, `pinctrl_pm_select_default_state`, `va_macro_runtime_resume`, `va_macro_hw_params` |
//! | [`VaMacro::start`] | DAPM stream start, after the AFE port started, before the front end | `fsgen_gate_enable`, `va_macro_enable_dec`, `va_macro_enable_dmic`, `va_macro_digital_mute(0)` |
//! | [`VaMacro::stop`] | BE hw_free, after the EOS, before the AFE port stops | `va_macro_digital_mute(1)`, DAPM stream stop: `va_macro_enable_dmic`, `va_macro_enable_dec`, `fsgen_gate_disable` |
//! | [`VaMacro::close`] | runtime suspend after the close | `va_macro_runtime_suspend` |

use alloc::vec::Vec;

use crate::rules;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// The VA macro, `0x62770000` on the laptop.
    Va,
    /// The LPI TLMM, `0x627C0000`.
    Lpi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// The whole register.
    Write {
        block: Block,
        offset: u32,
        value: u32,
    },
    /// Read it, put `value` in `mask`, write it back only if that changed it: regmap's path for a register it does not cache.
    Update {
        block: Block,
        offset: u32,
        mask: u32,
        value: u32,
    },
    /// Read it, put `value` in `mask`, write it back whatever it held: `lpi_gpio_set_mux`.
    Replace {
        block: Block,
        offset: u32,
        mask: u32,
        value: u32,
    },
    /// `usleep_range(us, ..)`: at least this long before the next.
    Delay { us: u32 },
}

impl Op {
    /// Where it goes: `None` for a delay.
    pub fn target(&self) -> Option<(Block, u32)> {
        match *self {
            Op::Write { block, offset, .. }
            | Op::Update { block, offset, .. }
            | Op::Replace { block, offset, .. } => Some((block, offset)),
            Op::Delay { .. } => None,
        }
    }

    /// Whether [`rules::va_access_allowed`] lets it through.
    pub fn allowed(&self) -> bool {
        self.target()
            .is_none_or(|(block, offset)| rules::va_access_allowed(block == Block::Lpi, offset))
    }
}

// lpass-va-macro.c
const MCLK_CONTROL: u32 = 0x000;
const MCLK_CONTROL_EN: u32 = 1 << 0;
const FS_CNT_CONTROL: u32 = 0x004;
const FS_CONTROL_EN: u32 = 1 << 0;
const FS_COUNTER_CLR: u32 = 1 << 1;
const SWR_CONTROL: u32 = 0x008;
const TOP_CFG0: u32 = 0x080;
const FS_BROADCAST_EN: u32 = 1 << 1;
const DMIC0_CTL: u32 = 0x084;
const DMIC_EN: u32 = 1 << 0;
const DMIC_CLK_SEL_MASK: u32 = 0b1110;
const DMIC_CLK_SEL_SHIFT: u32 = 1;
const DMIC_CFG: u32 = 0x094;
const RESET_ALL_DMICS: u32 = 1 << 7;
const DEBUG_BUS: u32 = 0x09C;
const DEBUG_EN: u32 = 0x0A0;
const TX_I2S_CTL: u32 = 0x0A4;
const I2S_CLK: u32 = 0x0A8;
const I2S_RESET: u32 = 0x0AC;
const CORE_ID_0: u32 = 0x0C0;
const CORE_ID_3: u32 = 0x0CC;
const SWR_MIC_CTL0: u32 = 0x0D0;
const SWR_MIC_CTL2: u32 = 0x0D8;
const SWR_CTRL: u32 = 0x0DC;
const ADC_MUX0_CFG0: u32 = 0x100;
const ADC_MUX_CFG_OFFSET: u32 = 0x8;
const ADC_MUX3_CFG1: u32 = 0x11C;
const TX0_PATH_CTL: u32 = 0x400;
const TX_PATH_OFFSET: u32 = 0x80;
const TX_PATH_CLK_EN: u32 = 1 << 5;
const TX_PATH_PGA_MUTE_EN: u32 = 1 << 4;
const PATH_CFG0: u32 = 0x04;
const ADC_DMIC_SEL_DMIC: u32 = 1 << 7;
const ADC_MODE_MASK: u32 = 0b110;
const HPF_CUT_OFF_MASK: u32 = 0b110_0000;
const CF_MIN_3DB_150HZ: u32 = 2;
const VOL_CTL: u32 = 0x0C;
const PATH_SEC2: u32 = 0x18;
const HPF_CUTOFF_FREQ_CHANGE_REQ: u32 = 1 << 1;
const HPF_ZERO_NO_GATE: u32 = 1 << 0;
const TX0_PATH_SEC7: u32 = 0x42C;
/// `VA_MAX_OFFSET`.
pub const MAX_REGISTER: u32 = 0x7A8;
const DECIMATORS: u32 = 4;
/// `va_macro_hw_params`: 48 kHz.
const TX_FS_48K: u32 = 4;

// pinctrl-lpass-lpi.h
const LPI_TLMM_REG_OFFSET: u32 = 0x1000;
const LPI_GPIO_CFG_REG: u32 = 0x00;
const LPI_GPIO_FUNCTION_MASK: u32 = 0b11_1100;
const LPI_GPIO_FUNCTION_SHIFT: u32 = 2;
/// `dmicN_clk` and `dmicN_data` are the first function after `gpio` on every DMIC pin of `sm8250_groups`.
const LPI_DMIC_FUNCTION: u32 = 1;
const LPI_GPIO_PULL_MASK: u32 = 0b11;
const LPI_GPIO_OUT_STRENGTH_MASK: u32 = 0b1_1100_0000;
const LPI_GPIO_OUT_STRENGTH_SHIFT: u32 = 6;
const LPI_GPIO_OE_MASK: u32 = 1 << 9;
const LPI_GPIO_VALUE_REG: u32 = 0x04;
const LPI_GPIO_BIAS_DISABLE: u32 = 0;
const LPI_GPIO_PULL_DOWN: u32 = 1;
/// `drive-strength = <8>` of the DMIC states Linux's boards use, as `LPI_GPIO_DS_TO_VAL` encodes it.
const LPI_DMIC_DRIVE_MA: u32 = 8;

/// `va_macro_probe` with no `qcom,dmic-sample-rate`, which this laptop's device tree does not have: `VA_MACRO_CLK_DIV_2`.
pub const DMIC_CLK_DIV_DEFAULT: u8 = 0;
/// `VA_MACRO_MCLK_FREQ`.
const VA_MCLK_HZ: u32 = 9_600_000;
/// `qcom,dmic-sample-rate` of Linux's SM8250 boards (`sm8250-hdk`, `qrb5165-rb5`).
pub const LINUX_BOARDS_DMIC_HZ: u32 = 600_000;

/// `va_macro_validate_dmic_sample_rate`: the `CLK_SEL` field for a DMIC clock rate, `None` for a rate the macro cannot make (Linux then
/// logs the error and keeps [`DMIC_CLK_DIV_DEFAULT`]).
pub const fn dmic_clk_div_for(rate: u32) -> Option<u8> {
    if rate == 0 || VA_MCLK_HZ % rate != 0 {
        return None;
    }
    match VA_MCLK_HZ / rate {
        2 => Some(0),
        3 => Some(1),
        4 => Some(2),
        6 => Some(3),
        8 => Some(4),
        16 => Some(5),
        _ => None,
    }
}

/// One DMIC into one decimator, on one pair of LPI pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mic {
    /// `VA DMIC<n>`, 0 to 7.
    pub dmic: u8,
    /// `VA DEC<n>`, 0 to 3.
    pub decimator: u8,
    pub clk_pin: u8,
    pub data_pin: u8,
    /// What the pins are set to besides their function.
    pub pins: PinStates,
}

impl Mic {
    pub const fn with_pins(self, pins: PinStates) -> Mic {
        Mic { pins, ..self }
    }

    /// The codec DMA lane this mic's decimator comes in on, as Windows computes it for a VA port: `1 << decimator`.
    pub const fn codec_dma_lane_mask(&self) -> u16 {
        1 << self.decimator
    }
}

/// The electrical part of the two pins' states, as `lpi_config_set` applies a pin state's properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinStates {
    /// The function only, as this laptop's device tree names it.
    FunctionOnly,
    /// Linux's DMIC states (`kodiak.dtsi`, `lpass_dmic01_clk` and `_data`): 8 mA, the clock with no bias, the data pulled down.
    LinuxBoards,
    /// What Windows' PEP sets on this laptop in the active state (DSDT `APCD`, component 10, `{0x0C,1,1,1,1,3,0,1}` and
    /// `{0x0D,1,1,0,1,3,0,1}`): 8 mA and pulled down, the clock an output driven high, the data an input.
    ThisLaptop,
}

/// One pin's state: `bias-*`, `drive-strength`, and `output-high`/`output-low` or input.
#[derive(Clone, Copy)]
struct PinState {
    pull: u32,
    drive_ma: u32,
    output: Option<bool>,
}

impl PinStates {
    /// The clock pin's state, then the data pin's.
    fn states(self) -> Option<[PinState; 2]> {
        match self {
            PinStates::FunctionOnly => None,
            PinStates::LinuxBoards => Some([
                PinState { pull: LPI_GPIO_BIAS_DISABLE, drive_ma: LPI_DMIC_DRIVE_MA, output: None },
                PinState { pull: LPI_GPIO_PULL_DOWN, drive_ma: LPI_DMIC_DRIVE_MA, output: None },
            ]),
            PinStates::ThisLaptop => Some([
                PinState { pull: LPI_GPIO_PULL_DOWN, drive_ma: LPI_DMIC_DRIVE_MA, output: Some(true) },
                PinState { pull: LPI_GPIO_PULL_DOWN, drive_ma: LPI_DMIC_DRIVE_MA, output: None },
            ]),
        }
    }
}

/// This laptop's internal microphone as Windows sets it up: ACDB device `0x0b`, whose codec record `0x20104` ("LPI_DMIC_6" in
/// `Codec_cal.acdb`) names DMIC5, the second of pair 2 on LPI gpio12 and 13, into VA decimator 2, with PEP component 10's pin states
/// and DMIC clock code 0 ([`DMIC_CLK_DIV_DEFAULT`]). `docs/research/juno-audio.md` section 21.
pub const INTERNAL_MIC: Mic = Mic { dmic: 5, decimator: 2, clk_pin: 12, data_pin: 13, pins: PinStates::ThisLaptop };

/// The first DMIC of a pin pair into decimator 0: pair 0 is gpio6 and 7 (`dmic01`, Linux's `VA DMIC0/1` on RB5 and the SC7280 boards),
/// 1 is gpio8 and 9 (`dmic23`), 2 is gpio12 and 13 (`dmic45`), the three DMIC states of this laptop's device tree.
pub const fn mic_on_pair(pair: u8) -> Option<Mic> {
    let (clk_pin, data_pin) = match pair {
        0 => (6, 7),
        1 => (8, 9),
        2 => (12, 13),
        _ => return None,
    };
    Some(Mic {
        dmic: pair * 2,
        decimator: 0,
        clk_pin,
        data_pin,
        pins: PinStates::FunctionOnly,
    })
}

/// `va_is_rw_register`.
fn writeable(reg: u32) -> bool {
    match reg {
        MCLK_CONTROL | FS_CNT_CONTROL | SWR_CONTROL | TOP_CFG0 => true,
        DMIC0_CTL..=DMIC_CFG | SWR_MIC_CTL0..=SWR_MIC_CTL2 => reg.is_multiple_of(4),
        DEBUG_BUS | DEBUG_EN | TX_I2S_CTL | I2S_CLK | I2S_RESET => true,
        ADC_MUX0_CFG0..=ADC_MUX3_CFG1 => reg.is_multiple_of(4),
        TX0_PATH_SEC7 => true,
        _ if (TX0_PATH_CTL..TX0_PATH_CTL + DECIMATORS * TX_PATH_OFFSET).contains(&reg) => {
            // CTL, CFG0, CFG1, VOL_CTL and SEC0 to SEC6 of each decimator.
            (reg - TX0_PATH_CTL) % TX_PATH_OFFSET <= 0x28 && reg.is_multiple_of(4)
        }
        _ => false,
    }
}

/// `va_is_readable_register`.
fn readable(reg: u32) -> bool {
    (CORE_ID_0..=CORE_ID_3).contains(&reg) && reg.is_multiple_of(4) || writeable(reg)
}

/// `va_is_volatile_register`, through `regmap_volatile`.
fn volatile(reg: u32) -> bool {
    readable(reg)
        && ((CORE_ID_0..=CORE_ID_3).contains(&reg) || (DMIC0_CTL..DMIC_CFG).contains(&reg))
}

/// `va_defaults`.
fn default_of(reg: u32) -> Option<u32> {
    let value = match reg {
        MCLK_CONTROL | FS_CNT_CONTROL | SWR_CONTROL | TOP_CFG0 => 0,
        DMIC0_CTL..DMIC_CFG => 0,
        DMIC_CFG => 0x80,
        DEBUG_BUS | DEBUG_EN | I2S_CLK | I2S_RESET => 0,
        TX_I2S_CTL => 0x0C,
        CORE_ID_0..=CORE_ID_3 => 0,
        SWR_MIC_CTL0..=SWR_MIC_CTL2 => 0xEE,
        SWR_CTRL => 0x06,
        ADC_MUX0_CFG0..=ADC_MUX3_CFG1 => 0,
        TX0_PATH_SEC7 => 0x25,
        _ if (TX0_PATH_CTL..TX0_PATH_CTL + DECIMATORS * TX_PATH_OFFSET).contains(&reg) => {
            match (reg - TX0_PATH_CTL) % TX_PATH_OFFSET {
                0x00 => 0x04,
                0x04 => 0x10,
                0x08 => 0x0B,
                0x18 => 0x01,
                0x1C => 0x3C,
                0x20 => 0x20,
                0x0C | 0x10 | 0x14 | 0x24 | 0x28 => 0,
                _ => return None,
            }
        }
        _ => return None,
    };
    reg.is_multiple_of(4).then_some(value)
}

const SLOTS: usize = (MAX_REGISTER / 4 + 1) as usize;

/// regmap for an MMIO map with `REGCACHE_FLAT`, as `va_regmap_config` makes it: the accesses that reach the hardware go to `ops`.
#[derive(Clone, Debug)]
struct Regmap {
    cache: [u32; SLOTS],
    cache_only: bool,
    cache_bypass: bool,
    cache_dirty: bool,
    no_sync_defaults: bool,
    ops: Vec<Op>,
}

impl Regmap {
    /// `regcache_flat_populate`: the defaults, no hardware access.
    fn new() -> Regmap {
        let mut cache = [0; SLOTS];
        for (i, slot) in cache.iter_mut().enumerate() {
            *slot = default_of(i as u32 * 4).unwrap_or(0);
        }
        Regmap {
            cache,
            cache_only: false,
            cache_bypass: false,
            cache_dirty: false,
            no_sync_defaults: false,
            ops: Vec::new(),
        }
    }

    fn slot(reg: u32) -> usize {
        (reg / 4) as usize
    }

    /// `_regmap_write`.
    fn write(&mut self, reg: u32, value: u32) {
        if reg > MAX_REGISTER || !writeable(reg) {
            return;
        }
        if !self.cache_bypass {
            if !volatile(reg) {
                self.cache[Self::slot(reg)] = value;
            }
            if self.cache_only {
                self.cache_dirty = true;
                return;
            }
        }
        self.ops.push(Op::Write {
            block: Block::Va,
            offset: reg,
            value,
        });
    }

    /// `_regmap_read` from the cache. A register it does not cache is read by [`Op::Update`] where it is used.
    fn read(&self, reg: u32) -> u32 {
        self.cache[Self::slot(reg)]
    }

    /// `_regmap_update_bits`.
    fn update_bits(&mut self, reg: u32, mask: u32, value: u32) {
        if volatile(reg) {
            // `_regmap_read` misses the cache: `-EBUSY` while cache-only, else the hardware's value.
            if !self.cache_only {
                self.ops.push(Op::Update {
                    block: Block::Va,
                    offset: reg,
                    mask,
                    value,
                });
            }
            return;
        }
        let orig = self.read(reg);
        let tmp = (orig & !mask) | (value & mask);
        if tmp != orig {
            self.write(reg, tmp);
        }
    }

    /// `regcache_reg_needs_sync`.
    fn needs_sync(&self, reg: u32, value: u32) -> bool {
        if !writeable(reg) {
            return false;
        }
        if !self.no_sync_defaults {
            return true;
        }
        default_of(reg) != Some(value)
    }

    /// `regcache_default_sync`.
    fn default_sync(&mut self, min: u32, max: u32) {
        let mut reg = min;
        while reg <= max {
            if !volatile(reg) && writeable(reg) {
                let value = self.read(reg);
                if self.needs_sync(reg, value) {
                    self.cache_bypass = true;
                    self.write(reg, value);
                    self.cache_bypass = false;
                }
            }
            reg += 4;
        }
    }

    /// `regcache_sync`.
    fn sync(&mut self) {
        let bypass = self.cache_bypass;
        if self.cache_dirty {
            self.default_sync(0, MAX_REGISTER);
            self.cache_dirty = false;
        }
        self.cache_bypass = bypass;
        self.no_sync_defaults = false;
    }

    /// `regcache_sync_region`: leaves the cache dirty.
    fn sync_region(&mut self, min: u32, max: u32) {
        let bypass = self.cache_bypass;
        if self.cache_dirty {
            self.default_sync(min, max);
        }
        self.cache_bypass = bypass;
        self.no_sync_defaults = false;
    }

    /// `regcache_mark_dirty`.
    fn mark_dirty(&mut self) {
        self.cache_dirty = true;
        self.no_sync_defaults = true;
    }
}

/// The VA macro and its pins, from probe on. Each phase returns what to do to the hardware, in order.
#[derive(Clone, Debug)]
pub struct VaMacro {
    map: Regmap,
    /// `va->dmic_clk_div`.
    dmic_clk_div: u8,
    /// `dmic_N_M_clk_cnt` and `dmic_N_M_clk_div`, per pair.
    pair_count: [i32; 4],
    pair_div: [u8; 4],
    /// `active_ch_mask` of the DAI in use.
    active: u8,
    /// `dec_mode`: `ADC_DEFAULT` for all, as nothing sets the control.
    dec_mode: [u8; DECIMATORS as usize],
    mic: Option<Mic>,
    started: bool,
    /// "VA_DEC<dec> Volume", in dB: 0 unless a program asks.
    gain_db: i32,
}

impl VaMacro {
    /// After `va_macro_probe` and the runtime autosuspend three seconds later: the cache holds the defaults, nothing was written.
    pub fn new(dmic_clk_div: u8) -> VaMacro {
        let mut va = VaMacro {
            map: Regmap::new(),
            dmic_clk_div,
            pair_count: [0; 4],
            pair_div: [0; 4],
            active: 0,
            dec_mode: [0; DECIMATORS as usize],
            mic: None,
            started: false,
            gain_db: 0,
        };
        va.close();
        va
    }

    fn take(&mut self) -> Vec<Op> {
        core::mem::take(&mut self.map.ops)
    }

    /// The `CLK_SEL` field the next DMIC enable uses, as if the device tree gave the rate [`dmic_clk_div_for`] turned into it.
    pub fn set_dmic_clk_div(&mut self, div: u8) {
        self.dmic_clk_div = div;
    }

    /// "VA_DEC<dec> Volume" for the next open, in dB, held to the control's range ([`crate::rules::mic_gain_reg`]).
    pub fn set_gain_db(&mut self, db: i32) {
        self.gain_db = db.clamp(crate::rules::MIC_GAIN_MIN_DB, crate::rules::MIC_GAIN_MAX_DB);
    }

    pub fn gain_db(&self) -> i32 {
        self.gain_db
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    /// The mixer controls a program sets before it opens the stream, the pins' default state, runtime resume and hw_params.
    pub fn open(&mut self, mic: Mic) -> Vec<Op> {
        let dec = u32::from(mic.decimator);
        // "VA DMIC MUX<dec>" = DMIC<n>: `va_macro_put_dec_enum`, then the enum's field.
        let mux_cfg0 = ADC_MUX0_CFG0 + ADC_MUX_CFG_OFFSET * dec;
        let item = u32::from(mic.dmic) + 1;
        self.map.update_bits(
            Self::tx(dec, PATH_CFG0),
            ADC_DMIC_SEL_DMIC,
            ADC_DMIC_SEL_DMIC,
        );
        self.map.update_bits(mux_cfg0, 0xF << 4, item << 4);
        // "VA DEC<dec> MUX" = VA_DMIC.
        self.map.update_bits(mux_cfg0 + 4, 1, 0);
        // "VA_DEC<dec> Volume": a cached write like the other controls, which the start writes back (`va_macro_enable_dec`).
        self.map.write(Self::tx(dec, VOL_CTL), crate::rules::mic_gain_reg(self.gain_db));
        // "VA_AIF<n>_CAP Mixer DEC<dec>" = 1: no register.
        self.active = 1 << dec;
        self.mic = Some(mic);
        // `__soc_pcm_open`: the pins first, then runtime PM.
        // Each pin's state in the map's order: its mux (`lpi_gpio_set_mux`), then its configuration (`lpi_config_set`).
        let states = mic.pins.states();
        for (i, pin) in [mic.clk_pin, mic.data_pin].into_iter().enumerate() {
            let offset = LPI_TLMM_REG_OFFSET * u32::from(pin) + LPI_GPIO_CFG_REG;
            self.map.ops.push(Op::Replace {
                block: Block::Lpi,
                offset,
                mask: LPI_GPIO_FUNCTION_MASK,
                value: LPI_DMIC_FUNCTION << LPI_GPIO_FUNCTION_SHIFT,
            });
            if let Some(state) = states.map(|s| s[i]) {
                // `lpi_config_set`: for an output the value goes first, "as per Hardware Programming Guide".
                if let Some(level) = state.output {
                    self.map.ops.push(Op::Write { block: Block::Lpi, offset: offset + LPI_GPIO_VALUE_REG, value: u32::from(level) << 1 });
                }
                self.map.ops.push(Op::Replace {
                    block: Block::Lpi,
                    offset,
                    mask: LPI_GPIO_PULL_MASK | LPI_GPIO_OUT_STRENGTH_MASK | LPI_GPIO_OE_MASK,
                    value: state.pull
                        | ((state.drive_ma / 2 - 1) << LPI_GPIO_OUT_STRENGTH_SHIFT)
                        | (u32::from(state.output.is_some()) << 9),
                });
            }
        }
        self.map.cache_only = false;
        self.map.sync();
        for d in self.active_decimators() {
            self.map.update_bits(Self::tx(d, 0), 0x0F, TX_FS_48K);
        }
        self.take()
    }

    /// DAPM's stream start: the clock and the frame sync, the decimator with its high-pass settle, the DMIC clock, then the unmute.
    pub fn start(&mut self) -> Vec<Op> {
        let Some(mic) = self.mic else {
            return Vec::new();
        };
        let dec = u32::from(mic.decimator);
        self.fs_gen_request(true);
        self.map.mark_dirty();
        self.map.sync_region(0, MAX_REGISTER);
        // `va_macro_enable_dec`, PRE_PMU then POST_PMU.
        let cfg0 = Self::tx(dec, PATH_CFG0);
        let sec2 = Self::tx(dec, PATH_SEC2);
        self.map.update_bits(
            cfg0,
            ADC_MODE_MASK,
            u32::from(self.dec_mode[dec as usize]) << 1,
        );
        self.map
            .update_bits(Self::tx(dec, 0), TX_PATH_CLK_EN, TX_PATH_CLK_EN);
        self.map.update_bits(sec2, HPF_ZERO_NO_GATE, 0);
        self.map.ops.push(Op::Delay { us: 1000 });
        if (self.map.read(cfg0) & HPF_CUT_OFF_MASK) >> 5 != CF_MIN_3DB_150HZ {
            self.map
                .update_bits(cfg0, HPF_CUT_OFF_MASK, CF_MIN_3DB_150HZ << 5);
            self.map
                .update_bits(sec2, HPF_CUTOFF_FREQ_CHANGE_REQ, HPF_CUTOFF_FREQ_CHANGE_REQ);
            self.map.ops.push(Op::Delay { us: 1000 });
            self.map.update_bits(sec2, HPF_CUTOFF_FREQ_CHANGE_REQ, 0);
        }
        self.map.ops.push(Op::Delay { us: 1000 });
        self.map
            .update_bits(sec2, HPF_ZERO_NO_GATE, HPF_ZERO_NO_GATE);
        self.map.ops.push(Op::Delay { us: 6000 });
        let vol = self.map.read(Self::tx(dec, VOL_CTL));
        self.map.write(Self::tx(dec, VOL_CTL), vol);
        // `va_macro_enable_dmic`, PRE_PMU.
        self.dmic_clk_enable(mic.dmic, true);
        self.digital_mute(false);
        self.started = true;
        self.take()
    }

    /// The mute at hw_free, then DAPM's stream stop. Nothing if [`Self::start`] has not run.
    pub fn stop(&mut self) -> Vec<Op> {
        let Some(mic) = self.mic.filter(|_| self.started) else {
            return Vec::new();
        };
        self.digital_mute(true);
        self.dmic_clk_enable(mic.dmic, false);
        self.map
            .update_bits(Self::tx(u32::from(mic.decimator), 0), TX_PATH_CLK_EN, 0);
        self.fs_gen_request(false);
        self.started = false;
        self.take()
    }

    /// `va_macro_runtime_suspend`: no access, the cache only from now until the next [`Self::open`].
    pub fn close(&mut self) {
        self.map.cache_only = true;
        self.map.mark_dirty();
    }

    fn tx(dec: u32, reg: u32) -> u32 {
        TX0_PATH_CTL + TX_PATH_OFFSET * dec + reg
    }

    fn active_decimators(&self) -> Vec<u32> {
        (0..DECIMATORS)
            .filter(|d| self.active & (1 << d) != 0)
            .collect()
    }

    /// `va_clk_rsc_fs_gen_request`.
    fn fs_gen_request(&mut self, enable: bool) {
        if enable {
            self.map
                .update_bits(MCLK_CONTROL, MCLK_CONTROL_EN, MCLK_CONTROL_EN);
            self.map.update_bits(
                FS_CNT_CONTROL,
                FS_CONTROL_EN | FS_COUNTER_CLR,
                FS_CONTROL_EN | FS_COUNTER_CLR,
            );
            self.map.update_bits(
                FS_CNT_CONTROL,
                FS_CONTROL_EN | FS_COUNTER_CLR,
                FS_CONTROL_EN,
            );
            self.map
                .update_bits(TOP_CFG0, FS_BROADCAST_EN, FS_BROADCAST_EN);
        } else {
            self.map.update_bits(MCLK_CONTROL, MCLK_CONTROL_EN, 0);
            self.map.update_bits(FS_CNT_CONTROL, FS_CONTROL_EN, 0);
            self.map.update_bits(TOP_CFG0, FS_BROADCAST_EN, 0);
        }
    }

    /// `va_macro_digital_mute`.
    fn digital_mute(&mut self, mute: bool) {
        for d in self.active_decimators() {
            self.map.update_bits(
                Self::tx(d, 0),
                TX_PATH_PGA_MUTE_EN,
                if mute { TX_PATH_PGA_MUTE_EN } else { 0 },
            );
        }
    }

    /// `va_dmic_clk_enable`.
    fn dmic_clk_enable(&mut self, dmic: u8, enable: bool) {
        if dmic > 7 {
            return;
        }
        let pair = usize::from(dmic / 2);
        let clk_reg = DMIC0_CTL + 4 * pair as u32;
        let freq_change = 1 << pair;
        let mut clk_div;
        if enable {
            clk_div = self.dmic_clk_div;
            self.pair_count[pair] += 1;
            if self.pair_count[pair] == 1 {
                self.map.update_bits(DMIC_CFG, RESET_ALL_DMICS, 0);
                self.map.update_bits(
                    clk_reg,
                    DMIC_CLK_SEL_MASK,
                    u32::from(clk_div) << DMIC_CLK_SEL_SHIFT,
                );
                self.map.update_bits(clk_reg, DMIC_EN, DMIC_EN);
            } else if self.pair_div[pair] > clk_div {
                self.change_div(clk_reg, freq_change, clk_div);
            } else {
                clk_div = self.pair_div[pair];
            }
        } else {
            self.pair_count[pair] -= 1;
            if self.pair_count[pair] == 0 {
                self.map.update_bits(clk_reg, DMIC_EN, 0);
                clk_div = 0;
                self.map.update_bits(
                    clk_reg,
                    DMIC_CLK_SEL_MASK,
                    u32::from(clk_div) << DMIC_CLK_SEL_SHIFT,
                );
            } else {
                clk_div = self.dmic_clk_div;
                if self.pair_div[pair] > clk_div {
                    self.change_div(clk_reg, freq_change, clk_div);
                } else {
                    clk_div = self.pair_div[pair];
                }
            }
        }
        self.pair_div[pair] = clk_div;
    }

    fn change_div(&mut self, clk_reg: u32, freq_change: u32, clk_div: u8) {
        self.map.update_bits(DMIC_CFG, freq_change, freq_change);
        self.map.update_bits(
            clk_reg,
            DMIC_CLK_SEL_MASK,
            u32::from(clk_div) << DMIC_CLK_SEL_SHIFT,
        );
        self.map.update_bits(DMIC_CFG, freq_change, 0);
    }
}
