//! Juno's screen-off standby, without its I/O: what a power key's samples mean, and how the backlight goes off and comes
//! back on the Galaxy Book Go.
//!
//! The backlight's brightness is a PWM of the PM6150L: LPG channel `0xbc00` on SID 5, found by reading the laptop
//! (`docs/research/juno-sleep.md`). The firmware's log says "SetBacklightByPWM: uLevel 00000100 Level 50" and that channel holds
//! 0x100 with its output on, while the SN65DSI86 bridge's PWM, which the device tree names, has a level of 0.
//!
//! Register meanings and the order of every write from Linux's `leds-qcom-lpg.c` (`lpg_apply`). The values are the firmware's,
//! read at takeover: only the output bit of the enable register changes, so the firmware's period and level return with the
//! same brightness. Nothing here touches the panel link: the firmware's
//! picture stays.
#![no_std]
#![forbid(unsafe_code)]

pub mod level;
pub mod quiet;
pub mod sleep_stats;
pub mod sleep_test;
pub mod wake;
pub mod wake_rules;

/// The channel: SID, and the base of its registers.
pub const LPG_SID: u8 = 5;
pub const LPG_BASE: u16 = 0xbc00;

const TYPE: u16 = 0x04;
/// PMIC peripheral type of an LPG, and this channel's subtype, from the laptop's own answer. A different pair is not this block.
const LPG_TYPE: [u8; 2] = [0x13, 0x0b];
/// `LPG_SIZE_CLK_REG`, `LPG_PREDIV_CLK_REG`: the PWM's resolution, clock and divider.
const SIZE_CLK: u16 = 0x41;
const PREDIV_CLK: u16 = 0x42;
/// `PWM_TYPE_CONFIG_REG` and its `LPG_ENABLE_GLITCH_REMOVAL`.
const TYPE_CONFIG: u16 = 0x43;
const GLITCH_REMOVAL: u8 = 1 << 5;
const PWM_VALUE: u16 = 0x44;
const ENABLE_CONTROL: u16 = 0x46;
const ENABLE_OUTPUT: u8 = 1 << 7;
const PWM_SYNC: u16 = 0x47;
const SYNC: u8 = 1 << 0;

/// The channel on its bus. Addresses are relative to [`LPG_BASE`].
pub trait Pmic {
    type Error;
    fn read(&mut self, offset: u16, out: &mut [u8]) -> Result<(), Self::Error>;
    fn write(&mut self, offset: u16, value: u8) -> Result<(), Self::Error>;
}

/// The channel's registers that decide the backlight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LpgState {
    pub id: [u8; 2],
    pub size_clk: u8,
    pub prediv_clk: u8,
    pub type_config: u8,
    pub value: u16,
    pub enable: u8,
}

impl LpgState {
    pub fn read<P: Pmic>(pmic: &mut P) -> Result<Self, P::Error> {
        let mut id = [0; 2];
        pmic.read(TYPE, &mut id)?;
        let mut clocks = [0; 3];
        pmic.read(SIZE_CLK, &mut clocks)?;
        let mut value = [0; 2];
        pmic.read(PWM_VALUE, &mut value)?;
        let mut enable = [0];
        pmic.read(ENABLE_CONTROL, &mut enable)?;
        Ok(Self {
            id,
            size_clk: clocks[0],
            prediv_clk: clocks[1],
            type_config: clocks[2],
            value: u16::from_le_bytes(value),
            enable: enable[0],
        })
    }

    /// Why this is not the backlight this code knows how to switch, if it is not.
    pub fn refusal(&self) -> Option<&'static str> {
        if self.id != LPG_TYPE {
            return Some("not the LPG channel seen on the Galaxy Book Go");
        }
        if self.value == 0 {
            return Some("the firmware left the level at 0");
        }
        None
    }
}

/// The backlight, once the firmware's state has been read. The bus is passed in: it is the caller's to lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Backlight {
    /// The enable register with the output on, as the firmware has it.
    running: u8,
    /// What `lpg_apply_freq` and `lpg_apply_pwm_value` write back: the firmware's.
    size_clk: u8,
    prediv_clk: u8,
    type_config: u8,
    value: u16,
    on: bool,
}

impl Backlight {
    /// Takes over the backlight as the last owner left it. Lit, it keeps the firmware's level and enable register. Dark (the
    /// output off: a kernel reloaded after the light went dark from idling or for sleep, since `lpg_apply` of a disabled channel
    /// clears only the output bit and leaves the PWM value), it is counted as off at the kept value, so [`Self::on`] lights it
    /// as a cold boot finds it, with the output bit set back; the kept PWM value is how the channel is told from an unset one.
    pub fn new(state: &LpgState) -> Result<Self, &'static str> {
        let lit = state.enable & ENABLE_OUTPUT != 0;
        match state.refusal() {
            Some(why) => Err(why),
            None => Ok(Self {
                running: state.enable | ENABLE_OUTPUT,
                size_clk: state.size_clk,
                prediv_clk: state.prediv_clk,
                type_config: state.type_config,
                value: state.value,
                on: lit,
            }),
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The channel's top PWM value, from its size (`level::pwm_max`).
    fn max(&self) -> u16 {
        level::pwm_max(self.size_clk)
    }

    /// The brightness, 0 to [`level::STEPS`]: what the firmware left until [`Self::set_level`].
    pub fn level(&self) -> u16 {
        level::level_for_value(self.value, self.max())
    }

    /// The PWM value given now, and the channel's top one.
    pub fn value(&self) -> (u16, u16) {
        (self.value, self.max())
    }

    /// A new brightness, 0 to [`level::STEPS`] (more counts as the top): Linux's `lpg_brightness_set`, which is `lpg_apply` with
    /// the new duty. While off it is only kept, and the next [`Self::on`] lights at it. Level 0 is the light off, as `pwm_bl.c`
    /// turns the PWM off at 0: `lpg_apply` of a disabled channel; the next non-zero level enables it again. A failed write leaves
    /// the old level.
    pub fn set_level<P: Pmic>(&mut self, pmic: &mut P, level: u16) -> Result<(), P::Error> {
        let before = self.value;
        self.value = level::value_for_level(level, self.max());
        if !self.on {
            return Ok(());
        }
        let result = self.apply(pmic, self.value != 0);
        if result.is_err() {
            self.value = before;
        }
        result
    }

    /// Off, if it is on: `lpg_apply` of a disabled channel (glitch removal on, the enable register with the output off, the
    /// sync, glitch removal off; `lpg_apply_freq` and `lpg_apply_pwm_value` write nothing for a disabled channel). A failed
    /// write leaves it counted as on, so the caller can say so and try again.
    pub fn off<P: Pmic>(&mut self, pmic: &mut P) -> Result<(), P::Error> {
        if self.on {
            self.apply(pmic, false)?;
            self.on = false;
        }
        Ok(())
    }

    /// On, if it is off: `lpg_apply` of an enabled channel. The enable alone is not enough: the laptop stayed dark until the
    /// sync (`lpg_apply_sync`, measured on the input current on 2026-11-02).
    pub fn on<P: Pmic>(&mut self, pmic: &mut P) -> Result<(), P::Error> {
        if !self.on {
            // A light set to level 0 stays dark (`set_level`).
            self.apply(pmic, self.value != 0)?;
            self.on = true;
        }
        Ok(())
    }

    /// `lpg_apply`, in its order. The LUT and SDAM steps do nothing for a channel without a pattern, as this one.
    fn apply<P: Pmic>(&self, pmic: &mut P, enabled: bool) -> Result<(), P::Error> {
        // lpg_disable_glitch: sets LPG_ENABLE_GLITCH_REMOVAL.
        pmic.write(TYPE_CONFIG, self.type_config | GLITCH_REMOVAL)?;
        if enabled {
            // lpg_apply_freq.
            pmic.write(SIZE_CLK, self.size_clk)?;
            pmic.write(PREDIV_CLK, self.prediv_clk)?;
            // lpg_apply_pwm_value: a two-byte bulk write, low byte first.
            self.write_value(pmic)?;
        }
        // lpg_apply_control.
        let control = if enabled {
            self.running
        } else {
            self.running & !ENABLE_OUTPUT
        };
        pmic.write(ENABLE_CONTROL, control)?;
        if enabled {
            // "Due to LPG hardware bug, in the PWM mode, having enabled PWM, We have to write PWM values one more time."
            self.write_value(pmic)?;
        }
        // lpg_apply_sync.
        pmic.write(PWM_SYNC, SYNC)?;
        // lpg_enable_glitch: clears LPG_ENABLE_GLITCH_REMOVAL.
        pmic.write(TYPE_CONFIG, self.type_config & !GLITCH_REMOVAL)
    }

    fn write_value<P: Pmic>(&self, pmic: &mut P) -> Result<(), P::Error> {
        let [low, high] = self.value.to_le_bytes();
        pmic.write(PWM_VALUE, low)?;
        pmic.write(PWM_VALUE + 1, high)
    }
}

/// The lid's hall sensor on the Galaxy Book Go: TLMM GPIO 93, high while open (Samsung's DSDT `GIO0`, pin 0x5D, `LIDB = LIDR`,
/// no pull; the same GPIO as xbill9's `gpio-keys/switch-lid`). Read by polling, as the power key is.
pub const LID_GPIO: usize = 93;

/// Samples in a row, 20 ms apart, before a lid change counts: Linux's `gpio-keys` waits out a 5 ms debounce by default; one
/// poll more than that here.
pub const LID_STABLE_SAMPLES: u8 = 2;

/// The lid switch, debounced: [`Lid::feed`] takes the GPIO's level and says when the state (closed) changes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lid {
    closed: Option<bool>,
    pending: Option<(bool, u8)>,
}

impl Lid {
    /// A sample of the GPIO (`true` = high = open). The first sample is the starting state, reported at once as `gpio-keys`
    /// reports its state at probe; after that a new state counts once it has been seen [`LID_STABLE_SAMPLES`] times in a row.
    pub fn feed(&mut self, level_high: bool) -> Option<bool> {
        let closed = !level_high;
        let Some(now) = self.closed else {
            self.closed = Some(closed);
            return Some(closed);
        };
        if closed == now {
            self.pending = None;
            return None;
        }
        let seen = match self.pending {
            Some((state, n)) if state == closed => n.saturating_add(1),
            _ => 1,
        };
        if seen >= LID_STABLE_SAMPLES {
            self.closed = Some(closed);
            self.pending = None;
            return Some(closed);
        }
        self.pending = Some((closed, seen));
        None
    }

    /// The lid's state, once a sample has been seen.
    pub fn is_closed(&self) -> Option<bool> {
        self.closed
    }
}

/// A power key read by polling its level: only changes count. The PMIC debounces the level (15.6 ms in the device tree).
#[derive(Clone, Copy, Debug, Default)]
pub struct Key {
    pressed: bool,
}

impl Key {
    /// A sample of the level. The new state, if it changed.
    pub fn feed(&mut self, pressed: bool) -> Option<bool> {
        (pressed != self.pressed).then(|| {
            self.pressed = pressed;
            pressed
        })
    }
}

/// What a press means. Pressing while asleep lights the screen at once, whoever reads the key next. A press while awake only
/// goes to user space: putting the machine to sleep is the shell's decision, so that without a shell the screen cannot go dark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OnPress {
    LightUp,
    TellUserSpace,
}

pub fn on_press(asleep: bool) -> OnPress {
    if asleep {
        OnPress::LightUp
    } else {
        OnPress::TellUserSpace
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// The channel as the firmware leaves it on the laptop (read there on 2026-11-01).
    struct Model {
        regs: [u8; 256],
        writes: Vec<(u16, u8)>,
        fail: bool,
    }

    impl Model {
        fn new() -> Self {
            let mut regs = [0; 256];
            regs[0x04] = 0x13;
            regs[0x05] = 0x0b;
            regs[0x41] = 0x07;
            regs[0x42] = 0x06;
            regs[0x44] = 0x00;
            regs[0x45] = 0x01;
            regs[0x46] = 0x80;
            Self {
                regs,
                writes: Vec::new(),
                fail: false,
            }
        }
    }

    impl Pmic for Model {
        type Error = ();

        fn read(&mut self, offset: u16, out: &mut [u8]) -> Result<(), ()> {
            if self.fail {
                return Err(());
            }
            let start = offset as usize;
            out.copy_from_slice(&self.regs[start..start + out.len()]);
            Ok(())
        }

        fn write(&mut self, offset: u16, value: u8) -> Result<(), ()> {
            if self.fail {
                return Err(());
            }
            self.regs[offset as usize] = value;
            self.writes.push((offset, value));
            Ok(())
        }
    }

    #[test]
    fn off_and_on_follow_lpg_apply_and_keep_the_level() {
        let mut lpg = Model::new();
        let before = LpgState::read(&mut lpg).unwrap();
        assert_eq!(before.value, 0x100);
        let mut light = Backlight::new(&before).unwrap();
        light.off(&mut lpg).unwrap();
        assert!(!light.is_on());
        assert_eq!(lpg.regs[0x46], 0);
        light.on(&mut lpg).unwrap();
        assert!(light.is_on());
        assert_eq!(LpgState::read(&mut lpg).unwrap(), before);
        // Linux lpg_apply for a disabled, then an enabled channel, with the firmware's values.
        assert_eq!(
            lpg.writes,
            [
                (0x43, 0x20),
                (0x46, 0),
                (0x47, 1),
                (0x43, 0),
                (0x43, 0x20),
                (0x41, 0x07),
                (0x42, 0x06),
                (0x44, 0x00),
                (0x45, 0x01),
                (0x46, 0x80),
                (0x44, 0x00),
                (0x45, 0x01),
                (0x47, 1),
                (0x43, 0)
            ]
        );
    }

    #[test]
    fn asking_twice_writes_once() {
        let mut lpg = Model::new();
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        light.on(&mut lpg).unwrap();
        assert!(lpg.writes.is_empty());
        light.off(&mut lpg).unwrap();
        light.off(&mut lpg).unwrap();
        assert_eq!(lpg.writes.len(), 4, "one lpg_apply");
    }

    #[test]
    fn the_other_bits_of_the_enable_register_survive() {
        let mut lpg = Model::new();
        lpg.regs[0x46] = 0xa4;
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        light.off(&mut lpg).unwrap();
        assert_eq!(lpg.regs[0x46], 0x24);
        light.on(&mut lpg).unwrap();
        assert_eq!(lpg.regs[0x46], 0xa4);
    }

    #[test]
    fn a_failed_write_leaves_it_counted_as_it_was() {
        let mut lpg = Model::new();
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        lpg.fail = true;
        assert!(light.off(&mut lpg).is_err());
        assert!(light.is_on());
        lpg.fail = false;
        light.off(&mut lpg).unwrap();
        lpg.fail = true;
        assert!(light.on(&mut lpg).is_err());
        assert!(!light.is_on());
        // A retry after a failure sends the whole sequence again.
        lpg.fail = false;
        lpg.writes.clear();
        light.on(&mut lpg).unwrap();
        assert_eq!(
            lpg.writes,
            [
                (0x43, 0x20),
                (0x41, 0x07),
                (0x42, 0x06),
                (0x44, 0x00),
                (0x45, 0x01),
                (0x46, 0x80),
                (0x44, 0x00),
                (0x45, 0x01),
                (0x47, 1),
                (0x43, 0)
            ]
        );
    }

    #[test]
    fn only_the_lpg_channel_registers_are_written() {
        let mut lpg = Model::new();
        lpg.regs[0x43] = 0x20;
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        light.off(&mut lpg).unwrap();
        light.on(&mut lpg).unwrap();
        assert!(lpg.writes.iter().all(|(o, _)| (0x41..=0x47).contains(o)));
        // lpg_enable_glitch clears the bit, whatever the firmware had.
        assert_eq!(lpg.regs[0x43], 0);
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        let good = LpgState::read(&mut Model::new()).unwrap();
        assert_eq!(good.refusal(), None);
        let mut other = good;
        other.id = [0x13, 0x11];
        assert!(Backlight::new(&other).is_err());
        let mut dark = good;
        dark.value = 0;
        assert!(Backlight::new(&dark).is_err());
    }

    #[test]
    fn only_changes_of_the_key_count() {
        let mut key = Key::default();
        assert_eq!(key.feed(false), None);
        assert_eq!(key.feed(true), Some(true));
        assert_eq!(key.feed(true), None);
        assert_eq!(key.feed(false), Some(false));
        assert_eq!(key.feed(false), None);
    }

    #[test]
    fn a_press_lights_up_only_a_sleeping_machine() {
        assert_eq!(on_press(true), OnPress::LightUp);
        assert_eq!(on_press(false), OnPress::TellUserSpace);
    }
    #[test]
    fn a_level_is_lpg_apply_with_the_new_duty() {
        let mut lpg = Model::new();
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        assert_eq!(light.level(), 50);
        assert_eq!(light.value(), (0x100, 511));
        light.set_level(&mut lpg, 40).unwrap();
        // 204 = 0xcc, low byte first, twice (Linux's hardware-bug rewrite), the output on, then the sync.
        let values: Vec<_> = lpg
            .writes
            .iter()
            .filter(|(r, _)| *r == 0x44 || *r == 0x45)
            .copied()
            .collect();
        assert_eq!(values, [(0x44, 204), (0x45, 0), (0x44, 204), (0x45, 0)]);
        let sync = lpg.writes.iter().position(|w| *w == (0x47, 1)).unwrap();
        let enable = lpg.writes.iter().position(|w| *w == (0x46, 0x80)).unwrap();
        assert!(enable < sync);
        assert_eq!(lpg.regs[0x44], 204);
        assert_eq!(light.level(), 40);
        // Only this channel's own registers.
        assert!(lpg.writes.iter().all(|(r, _)| (0x41..=0x47).contains(r)));
    }

    #[test]
    fn a_kernel_reloaded_in_the_dark_takes_the_light_over_off_at_the_kept_level() {
        let mut lpg = Model::new();
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        // Dark from idling, then a reload: the new kernel reads the channel with the output off and the old value.
        light.set_level(&mut lpg, 0).unwrap();
        let dark = LpgState::read(&mut lpg).unwrap();
        assert_eq!((dark.enable, dark.value), (0x00, 0x100));
        let mut light = Backlight::new(&dark).unwrap();
        assert!(!light.is_on(), "counted as off");
        assert_eq!(light.level(), 50, "at the kept value");
        // on() lights it as a cold boot finds it: the firmware's enable register and level.
        light.on(&mut lpg).unwrap();
        assert_eq!(lpg.regs[0x46], 0x80);
        assert_eq!((lpg.regs[0x44], lpg.regs[0x45]), (0x00, 0x01));
        // Off for sleep, then a reload: the same.
        let mut slept = Backlight::new(&state).unwrap();
        let mut other = Model::new();
        slept.off(&mut other).unwrap();
        let found = Backlight::new(&LpgState::read(&mut other).unwrap()).unwrap();
        assert!(!found.is_on());
        assert_eq!(found.level(), 50);
    }

    #[test]
    fn level_zero_turns_the_output_off_and_off_keeps_a_new_level_for_on() {
        let mut lpg = Model::new();
        let state = LpgState::read(&mut lpg).unwrap();
        let mut light = Backlight::new(&state).unwrap();
        light.set_level(&mut lpg, 0).unwrap();
        assert_eq!(lpg.regs[0x46] & 0x80, 0, "output off at level 0");
        assert_eq!(light.level(), 0);
        light.set_level(&mut lpg, 30).unwrap();
        assert_eq!(lpg.regs[0x46], 0x80);
        assert_eq!(lpg.regs[0x44], 153);
        // Off: a level is kept and written by on().
        light.off(&mut lpg).unwrap();
        lpg.writes.clear();
        light.set_level(&mut lpg, 100).unwrap();
        assert!(lpg.writes.is_empty(), "nothing written while off");
        light.on(&mut lpg).unwrap();
        assert_eq!((lpg.regs[0x44], lpg.regs[0x45]), (0xff, 0x01));
        // A light at 0 stays dark through off and on.
        light.set_level(&mut lpg, 0).unwrap();
        light.off(&mut lpg).unwrap();
        light.on(&mut lpg).unwrap();
        assert_eq!(lpg.regs[0x46] & 0x80, 0);
    }

    #[test]
    fn the_first_lid_sample_is_the_starting_state() {
        let mut lid = Lid::default();
        assert_eq!(lid.is_closed(), None);
        assert_eq!(lid.feed(true), Some(false));
        assert_eq!(lid.is_closed(), Some(false));
        let mut lid = Lid::default();
        assert_eq!(lid.feed(false), Some(true));
    }

    #[test]
    fn a_lid_change_counts_after_two_equal_samples() {
        let mut lid = Lid::default();
        lid.feed(true);
        assert_eq!(lid.feed(false), None);
        assert_eq!(lid.feed(false), Some(true));
        assert_eq!(lid.feed(false), None);
        assert_eq!(lid.feed(true), None);
        assert_eq!(lid.feed(true), Some(false));
    }

    #[test]
    fn a_one_sample_glitch_is_ignored() {
        let mut lid = Lid::default();
        lid.feed(true);
        assert_eq!(lid.feed(false), None);
        assert_eq!(lid.feed(true), None);
        assert_eq!(lid.feed(false), None);
        assert_eq!(lid.feed(true), None);
        assert_eq!(lid.is_closed(), Some(false));
    }
}
