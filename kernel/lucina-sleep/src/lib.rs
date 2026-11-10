// SPDX-License-Identifier: MPL-2.0

//! Juno's sleep as a sequence of steps (`docs/research/juno-power.md`, "Sleep"): what is suspended on the way in, in which
//! order, what is resumed on the way out, and what a wake in the middle does. The kernel's power driver asks [`next`] what to
//! do, does it, and reports with [`after`]; it keeps no order of its own.
//!
//! The original is Linux's suspend to idle, which is how Linux sleeps on SC7180 (no PSCI `SYSTEM_SUSPEND`):
//! `kernel/power/suspend.c` (`suspend_devices_and_enter`, `suspend_enter`, `s2idle_loop`) and `drivers/base/power/main.c`
//! (`dpm_suspend` walks the devices in order and moves each to the "suspended" list; `dpm_resume` walks that list from its end,
//! so devices come back in reverse; a device whose suspend fails, or `pm_wakeup_pending()` during the walk, makes
//! `dpm_suspend` return an error and the caller resumes what was suspended: `suspend_enter` gives `-EBUSY`). The CPUs go to
//! their deepest idle state last (`s2idle_enter` → `cpuidle_enter_s2idle`), after the wake sources are armed
//! (`dpm_suspend_noirq` → `device_wakeup_arm_wake_irqs`).
//!
//! Verus proves this very file (`verification/juno-sleep/rules.rs`, D-011). Proved there, over every sequence of inputs:
//! 1. every step suspended is resumed exactly once, in reverse order, before the sequence is awake again;
//! 2. the CPUs' deepest idle is only entered after the wake source was found armed;
//! 3. a wake during the suspend stops it: nothing more is suspended, and what was suspended is resumed;
//! 4. the steps suspended are always the first `done` of [`ORDER`];
//! 5. the sequence ends: at most `2 * STEPS` actions between being told to sleep and being asleep or awake again;
//! 6. the PDC words the wake setup may write are only the four wake pins' type and enable ([`pdc_write_allowed`]).
//!
//! Not proved: that the kernel runs the loop the model runs, and that the devices do what they are told.
#![no_std]
#![forbid(unsafe_code)]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The steps, in the order they are suspended.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The panel's light off (the LPG channel), as DRM's suspend switches the panel off. Resumed: on again.
    Light,
    /// Both I2C-HID devices to SET_POWER SLEEP (`i2c_hid_core_suspend`), each acknowledged by its thread. Resumed: ON.
    Input,
    /// The GPU down (`adreno_system_suspend`). Resumed: allowed up again (`adreno_system_resume`; the next batch powers it).
    Gpu,
    /// The wake sources are armed: the power key and the lid (their interrupts, or the poll as a fallback). Nothing to undo.
    WakeArmed,
    /// Idle CPUs take their deepest state with the tick stopped (`cpuidle_enter_s2idle`). Resumed: the governor chooses again.
    CpuIdle,
}

/// How many steps there are.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const STEPS: u8 = 5;

/// The step suspended `i`-th (0-based).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        i < STEPS,
    ensures
        r == order(i),
))]
pub fn step_at(i: u8) -> Step {
    if i == 0 {
        Step::Light
    } else if i == 1 {
        Step::Input
    } else if i == 2 {
        Step::Gpu
    } else if i == 3 {
        Step::WakeArmed
    } else {
        Step::CpuIdle
    }
}

/// Where `s` is in the order.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r < STEPS,
        order(r) == s,
))]
pub fn index_of(s: Step) -> u8 {
    match s {
        Step::Light => 0,
        Step::Input => 1,
        Step::Gpu => 2,
        Step::WakeArmed => 3,
        Step::CpuIdle => 4,
    }
}

/// Where the sequence is.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Awake,
    Suspending,
    Asleep,
    /// Coming back; `aborted` when the sleep never got there (a wake during the suspend, a step that failed, nothing armed).
    Resuming { aborted: bool },
}

/// The sequence: the first `done` steps of the order are suspended.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sleep {
    pub done: u8,
    pub phase: Phase,
}

/// What the kernel does next.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Suspend(Step),
    Resume(Step),
    /// Nothing: asleep, or awake.
    Idle,
}

/// What the kernel reports each time it asks.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inputs {
    /// A wake is pending: the power key, the lid, the alarm, or the shell's unblank (`pm_wakeup_pending`).
    pub wake: bool,
    /// A wake source is armed. Only the [`Step::WakeArmed`] step reads it.
    pub armed: bool,
}

/// Awake, nothing suspended.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const AWAKE: Sleep = Sleep { done: 0, phase: Phase::Awake };

/// Told to sleep: an awake sequence starts suspending; any other is left as it is.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        s.phase == Phase::Awake ==> r == (Sleep { done: s.done, phase: Phase::Suspending }),
        s.phase != Phase::Awake ==> r == s,
))]
pub fn start(s: Sleep) -> Sleep {
    match s.phase {
        Phase::Awake => Sleep { done: s.done, phase: Phase::Suspending },
        _ => s,
    }
}

/// The step that comes back first: the last one suspended. `done` must be at least 1.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        1 <= done <= STEPS,
    ensures
        r == Action::Resume(order((done - 1) as u8)),
))]
fn resume_top(done: u8) -> Action {
    Action::Resume(step_at(done - 1))
}

/// What to do next, and the sequence after deciding it. See [`after`] for the sequence after doing it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        valid(s),
    ensures
        valid(r.0),
        next_spec(s, inp, r.0, r.1),
))]
pub fn next(s: Sleep, inp: Inputs) -> (Sleep, Action) {
    match s.phase {
        Phase::Awake => (s, Action::Idle),
        Phase::Suspending => {
            // `pm_wakeup_pending()` in the walk, or before the CPUs idle (`s2idle_loop`), or nothing armed: what was suspended
            // comes back.
            if inp.wake || (s.done == index_of(Step::WakeArmed) && !inp.armed) {
                if s.done == 0 {
                    (AWAKE, Action::Idle)
                } else {
                    (Sleep { done: s.done, phase: Phase::Resuming { aborted: true } }, resume_top(s.done))
                }
            } else {
                if s.done >= STEPS {
                    (Sleep { done: s.done, phase: Phase::Asleep }, Action::Idle)
                } else {
                    (s, Action::Suspend(step_at(s.done)))
                }
            }
        }
        Phase::Asleep => {
            if inp.wake && s.done >= 1 {
                (Sleep { done: s.done, phase: Phase::Resuming { aborted: false } }, resume_top(s.done))
            } else {
                (s, Action::Idle)
            }
        }
        Phase::Resuming { .. } => {
            if s.done == 0 {
                (AWAKE, Action::Idle)
            } else {
                (s, resume_top(s.done))
            }
        }
    }
}

/// The sequence after the kernel did `a`, which [`next`] gave for `s`; `ok` says whether the device did it. A suspend that fails
/// counts as not suspended and turns the sleep back (`dpm_suspend` fails, `dpm_resume` follows). A resume is counted whatever
/// happened (Linux logs a failed resume and goes on), so the sequence always ends.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        valid(s),
    ensures
        valid(r),
        after_spec(s, a, ok, r),
))]
pub fn after(s: Sleep, a: Action, ok: bool) -> Sleep {
    match s.phase {
        Phase::Suspending => match a {
            Action::Suspend(x) if s.done < STEPS && index_of(x) == s.done => {
                if ok {
                    Sleep { done: s.done + 1, phase: Phase::Suspending }
                } else if s.done == 0 {
                    AWAKE
                } else {
                    Sleep { done: s.done, phase: Phase::Resuming { aborted: true } }
                }
            }
            _ => s,
        },
        Phase::Resuming { aborted } => match a {
            Action::Resume(x) if s.done >= 1 && index_of(x) == s.done - 1 => {
                Sleep { done: s.done - 1, phase: Phase::Resuming { aborted } }
            }
            _ => s,
        },
        _ => s,
    }
}

/// Whether `step` is suspended now.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        valid(s),
    ensures
        r == (index_of_spec(step) < s.done),
))]
pub fn suspended(s: Sleep, step: Step) -> bool {
    index_of(step) < s.done
}

/// The PDC's wake pins on this laptop: the SPMI arbiter's summary interrupt (the device tree's `spmi@c440000`:
/// `interrupts-extended = <&pdc 1 IRQ_TYPE_LEVEL_HIGH>`) and the lid's GPIO 93 (`pinctrl-sc7180.c` `sc7180_pdc_map`: `{93, 49}`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SPMI_PIN: u32 = 1;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const LID_PIN: u32 = 49;
/// The keyboard's and the touchpad's interrupt lines, which this laptop's DSDT declares `GpioInt (Level, ActiveLow,
/// SharedAndWake, PullUp)` (`ECKB` on GPIO 104, `TCPD` on GPIO 31): `sc7180_pdc_map` `{104, 67}` and `{31, 33}`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEYBOARD_PIN: u32 = 67;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TOUCHPAD_PIN: u32 = 33;
/// The USB-C PD controllers' interrupt lines (Samsung's DSDT, `EMEC`: `GpioInt (Level, ActiveLow, ExclusiveAndWake, PullUp)` on
/// GIO0 pins 0x200 and 0x240, the PD controllers of `I2C8` and `I2C3` in EmuEC.sys's interrupt order). GIO0's virtual pins are
/// 0x40 x the index of its `_CRS` Interrupt entry: index 8 is GSIV 0x22A (SPI 522, PDC 42 = GPIO 4), index 9 GSIV 0x232
/// (SPI 530, PDC 50 = GPIO 3), `qcom,pdc-ranges = <0 480 94>`, `sc7180_pdc_map` `{4, 42}` and `{3, 50}`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PDIC1_PIN: u32 = 42;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PDIC0_PIN: u32 = 50;
/// `qcom-pdc.c`: `IRQ_ENABLE_BANK` (0x10, one bit per pin before version 3.2), `IRQ_i_CFG` (0x110 + 4 * pin), and the version
/// from which the enable is `IRQ_i_CFG` bit 3 instead (`PDC_VERSION_3_2`, 0x30200).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IRQ_ENABLE_BANK: u32 = 0x10;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IRQ_I_CFG: u32 = 0x110;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PDC_VERSION_3_2: u32 = 0x30200;

/// Whether the wake setup may write the PDC word at `offset`: a wake pin's `IRQ_i_CFG`, or, before version 3.2, the
/// `IRQ_ENABLE_BANK` word that holds its enable bit. Nothing else in the PDC.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == pdc_allowed_spec(version, offset),
))]
pub fn pdc_write_allowed(version: u32, offset: u32) -> bool {
    let cfg_a = IRQ_I_CFG + 4 * SPMI_PIN;
    let cfg_b = IRQ_I_CFG + 4 * LID_PIN;
    let cfg_c = IRQ_I_CFG + 4 * KEYBOARD_PIN;
    let cfg_d = IRQ_I_CFG + 4 * TOUCHPAD_PIN;
    let cfg_e = IRQ_I_CFG + 4 * PDIC1_PIN;
    let cfg_f = IRQ_I_CFG + 4 * PDIC0_PIN;
    let bank_a = IRQ_ENABLE_BANK + 4 * (SPMI_PIN / 32);
    let bank_b = IRQ_ENABLE_BANK + 4 * (LID_PIN / 32);
    let bank_c = IRQ_ENABLE_BANK + 4 * (KEYBOARD_PIN / 32);
    let bank_d = IRQ_ENABLE_BANK + 4 * (TOUCHPAD_PIN / 32);
    let bank_e = IRQ_ENABLE_BANK + 4 * (PDIC1_PIN / 32);
    let bank_f = IRQ_ENABLE_BANK + 4 * (PDIC0_PIN / 32);
    offset == cfg_a
        || offset == cfg_b
        || offset == cfg_c
        || offset == cfg_d
        || offset == cfg_e
        || offset == cfg_f
        || (version < PDC_VERSION_3_2 && (offset == bank_a || offset == bank_b || offset == bank_c || offset == bank_d || offset == bank_e || offset == bank_f))
}

#[cfg(verus_keep_ghost)]
verus! {

/// The order, as a spec.
pub open spec fn order(i: u8) -> Step {
    if i == 0 { Step::Light }
    else if i == 1 { Step::Input }
    else if i == 2 { Step::Gpu }
    else if i == 3 { Step::WakeArmed }
    else { Step::CpuIdle }
}

pub open spec fn index_of_spec(s: Step) -> u8 {
    match s {
        Step::Light => 0,
        Step::Input => 1,
        Step::Gpu => 2,
        Step::WakeArmed => 3,
        Step::CpuIdle => 4,
    }
}

/// The states that can occur: awake has nothing suspended, asleep has everything.
pub open spec fn valid(s: Sleep) -> bool {
    &&& s.done <= STEPS
    &&& s.phase == Phase::Awake ==> s.done == 0
    &&& s.phase == Phase::Asleep ==> s.done == STEPS
}

/// What `next` does, as a relation.
pub open spec fn next_spec(s: Sleep, inp: Inputs, r: Sleep, a: Action) -> bool {
    match s.phase {
        Phase::Awake => r == s && a == Action::Idle,
        Phase::Suspending =>
            if inp.wake || (s.done == index_of_spec(Step::WakeArmed) && !inp.armed) {
                if s.done == 0 { r == AWAKE && a == Action::Idle }
                else {
                    r == (Sleep { done: s.done, phase: Phase::Resuming { aborted: true } })
                        && a == Action::Resume(order((s.done - 1) as u8))
                }
            } else if s.done >= STEPS {
                r == (Sleep { done: s.done, phase: Phase::Asleep }) && a == Action::Idle
            } else {
                r == s && a == Action::Suspend(order(s.done))
            },
        Phase::Asleep =>
            if inp.wake && s.done >= 1 {
                r == (Sleep { done: s.done, phase: Phase::Resuming { aborted: false } })
                    && a == Action::Resume(order((s.done - 1) as u8))
            } else {
                r == s && a == Action::Idle
            },
        Phase::Resuming { aborted } =>
            if s.done == 0 { r == AWAKE && a == Action::Idle }
            else { r == s && a == Action::Resume(order((s.done - 1) as u8)) },
    }
}

/// What `after` does, as a relation.
pub open spec fn after_spec(s: Sleep, a: Action, ok: bool, r: Sleep) -> bool {
    match s.phase {
        Phase::Suspending => match a {
            Action::Suspend(x) =>
                if s.done < STEPS && index_of_spec(x) == s.done {
                    if ok { r == (Sleep { done: (s.done + 1) as u8, phase: Phase::Suspending }) }
                    else if s.done == 0 { r == AWAKE }
                    else { r == (Sleep { done: s.done, phase: Phase::Resuming { aborted: true } }) }
                } else { r == s },
            _ => r == s,
        },
        Phase::Resuming { aborted } => match a {
            Action::Resume(x) =>
                if s.done >= 1 && index_of_spec(x) == s.done - 1 {
                    r == (Sleep { done: (s.done - 1) as u8, phase: Phase::Resuming { aborted } })
                } else { r == s },
            _ => r == s,
        },
        _ => r == s,
    }
}

pub open spec fn pdc_allowed_spec(version: u32, offset: u32) -> bool {
    offset == 0x110 + 4 * 1 || offset == 0x110 + 4 * 49 || offset == 0x110 + 4 * 67 || offset == 0x110 + 4 * 33
        || offset == 0x110 + 4 * 42 || offset == 0x110 + 4 * 50
        || (version < 0x30200 && (offset == 0x10 || offset == 0x10 + 4 * 1 || offset == 0x10 + 4 * 2))
}

} // verus!

#[cfg(test)]
mod tests;
