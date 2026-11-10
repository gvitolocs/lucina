//! A TLMM pad's control register in the state a device tree `pinctrl` node asks for, for a pad used as a GPIO input
//! (an interrupt line read as a level). Field layout from Linux's `pinctrl-sc7180.c` (`PINGROUP`: `pull_bit` 0,
//! `mux_bit` 2, `drv_bit` 6, `oe_bit` 9), values from `pinctrl-msm.c` (`msm_config_reg`, `msm_config_group_set`).
//! The write is a read-modify-write of the mux, pull and drive fields only, as `msm_pinmux_set_mux` and
//! `msm_config_group_set` do (`pinctrl-msm.c:539-543`): every other bit of the register is kept.

const PULL_MASK: u32 = 0x3;
const MUX_MASK: u32 = 0xf << 2;
const DRIVE_SHIFT: u32 = 6;
const DRIVE_MASK: u32 = 0x7 << DRIVE_SHIFT;
const OE: u32 = 1 << 9;

/// A `bias-*` property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    /// `bias-disable`.
    Disable,
    /// `bias-pull-down`.
    PullDown,
    /// `bias-pull-up`.
    PullUp,
}

/// What a `pinctrl` state with `function = "gpio"` says; what it does not say stays as it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpioInput {
    pub bias: Option<Bias>,
    /// `drive-strength`, in mA: even, from 2 to 16. Anything else is ignored.
    pub drive_ma: Option<u32>,
}

impl GpioInput {
    /// The control register `now` with this state applied: GPIO function, output driver off (as
    /// `msm_gpio_direction_input` clears it), the pull and drive fields set, every other bit kept.
    pub fn ctl(&self, now: u32) -> u32 {
        let mut ctl = now & !(MUX_MASK | OE);
        match self.bias {
            Some(Bias::Disable) => ctl &= !PULL_MASK,
            Some(Bias::PullDown) => ctl = (ctl & !PULL_MASK) | 1,
            Some(Bias::PullUp) => ctl = (ctl & !PULL_MASK) | 3,
            None => {}
        }
        if let Some(ma) = self
            .drive_ma
            .filter(|ma| (2..=16).contains(ma) && ma % 2 == 0)
        {
            ctl = (ctl & !DRIVE_MASK) | ((ma / 2 - 1) << DRIVE_SHIFT);
        }
        ctl
    }
}
