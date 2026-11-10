extern crate std;
use std::vec::Vec;

use super::*;

const ALL: [Step; STEPS as usize] = [Step::Light, Step::Input, Step::Gpu, Step::WakeArmed, Step::CpuIdle];
const QUIET: Inputs = Inputs { wake: false, armed: true };
const WAKE: Inputs = Inputs { wake: true, armed: true };

/// Runs the loop as the kernel does: ask, do (with `ok` from `outcome`), report; returns the actions until Idle.
fn run(mut s: Sleep, inp: impl Fn(&Sleep) -> Inputs, outcome: impl Fn(Action) -> bool) -> (Sleep, Vec<Action>) {
    let mut actions = Vec::new();
    for _ in 0..=2 * STEPS {
        let (s1, a) = next(s, inp(&s));
        if a == Action::Idle {
            return (s1, actions);
        }
        s = after(s1, a, outcome(a));
        actions.push(a);
    }
    panic!("no end after {} actions: {:?}", 2 * STEPS, actions);
}

fn suspends(actions: &[Action]) -> Vec<Step> {
    actions.iter().filter_map(|a| if let Action::Suspend(x) = a { Some(*x) } else { None }).collect()
}

fn resumes(actions: &[Action]) -> Vec<Step> {
    actions.iter().filter_map(|a| if let Action::Resume(x) = a { Some(*x) } else { None }).collect()
}

#[test]
fn the_order_round_trips() {
    for (i, step) in ALL.iter().enumerate() {
        assert_eq!(step_at(i as u8), *step);
        assert_eq!(index_of(*step), i as u8);
    }
}

#[test]
fn a_sleep_suspends_in_order_and_a_wake_resumes_in_reverse() {
    let (asleep, actions) = run(start(AWAKE), |_| QUIET, |_| true);
    assert_eq!(asleep, Sleep { done: STEPS, phase: Phase::Asleep });
    assert_eq!(suspends(&actions), ALL);
    assert!(resumes(&actions).is_empty());
    let (awake, actions) = run(asleep, |s| Inputs { wake: s.phase == Phase::Asleep, armed: true }, |_| true);
    assert_eq!(awake, AWAKE);
    let mut reverse = ALL;
    reverse.reverse();
    assert_eq!(resumes(&actions), reverse);
}

#[test]
fn a_wake_at_each_point_of_the_suspend_resumes_what_was_suspended() {
    // Up to and including the moment the last step is done: s2idle_loop checks for a wake before the CPUs idle.
    for at in 0..=STEPS {
        let (s, actions) = run(start(AWAKE), |s| Inputs { wake: s.done == at, armed: true }, |_| true);
        assert_eq!(s, AWAKE, "wake at {}", at);
        let done = suspends(&actions);
        assert_eq!(done, ALL[..at as usize]);
        let mut back = done.clone();
        back.reverse();
        assert_eq!(resumes(&actions), back);
    }
}

#[test]
fn a_failed_suspend_at_each_step_turns_back() {
    for at in 0..STEPS {
        let (s, actions) = run(start(AWAKE), |_| QUIET, |a| a != Action::Suspend(step_at(at)));
        assert_eq!(s, AWAKE, "failure at {}", at);
        // The failed step was asked for but is not resumed; the ones before it are, in reverse.
        assert_eq!(suspends(&actions), ALL[..=at as usize]);
        let mut back = ALL[..at as usize].to_vec();
        back.reverse();
        assert_eq!(resumes(&actions), back);
    }
}

#[test]
fn nothing_armed_never_idles_the_cpus() {
    let (s, actions) = run(start(AWAKE), |_| Inputs { wake: false, armed: false }, |_| true);
    assert_eq!(s, AWAKE);
    assert_eq!(suspends(&actions), [Step::Light, Step::Input, Step::Gpu]);
    assert_eq!(resumes(&actions), [Step::Gpu, Step::Input, Step::Light]);
}

#[test]
fn a_wake_while_resuming_changes_nothing() {
    let (asleep, _) = run(start(AWAKE), |_| QUIET, |_| true);
    let (s, actions) = run(asleep, |_| WAKE, |_| false);
    assert_eq!(s, AWAKE);
    assert_eq!(resumes(&actions).len(), STEPS as usize);
}

#[test]
fn start_only_starts_from_awake() {
    let asleep = Sleep { done: STEPS, phase: Phase::Asleep };
    assert_eq!(start(asleep), asleep);
    assert_eq!(start(AWAKE), Sleep { done: 0, phase: Phase::Suspending });
}

#[test]
fn an_action_that_was_not_asked_for_changes_nothing() {
    let s = Sleep { done: 2, phase: Phase::Suspending };
    assert_eq!(after(s, Action::Suspend(Step::Light), true), s);
    assert_eq!(after(s, Action::Resume(Step::Input), true), s);
    let r = Sleep { done: 2, phase: Phase::Resuming { aborted: false } };
    assert_eq!(after(r, Action::Resume(Step::Light), true), r);
    assert_eq!(after(r, Action::Suspend(Step::Gpu), true), r);
}

#[test]
fn suspended_reads_the_prefix() {
    let s = Sleep { done: 3, phase: Phase::Suspending };
    assert!(suspended(s, Step::Gpu));
    assert!(!suspended(s, Step::WakeArmed));
}

#[test]
fn the_pdc_words_are_the_two_wake_pins() {
    let old = 0x30100;
    let new = 0x30200;
    for offset in [0x114, 0x1d4] {
        assert!(pdc_write_allowed(old, offset));
        assert!(pdc_write_allowed(new, offset));
    }
    // The enable banks: pin 1 in word 0, pin 49 in word 1; only before 3.2.
    for offset in [0x10, 0x14] {
        assert!(pdc_write_allowed(old, offset));
        assert!(!pdc_write_allowed(new, offset));
    }
    // 0x1d8 is pin 50's (the second PD controller's), so the lid's other neighbour is 0x1d0.
    for offset in [0x0, 0x110, 0x118, 0x1d0, 0x1dc, 0x1000] {
        assert!(!pdc_write_allowed(old, offset), "{:#x}", offset);
    }
}

#[test]
fn the_keyboard_and_touchpad_pins_too() {
    let old = 0x30100;
    let new = 0x30200;
    // IRQ_i_CFG of pin 67 (keyboard, GPIO 104) and pin 33 (touchpad, GPIO 31).
    for offset in [0x21c, 0x194] {
        assert!(pdc_write_allowed(old, offset));
        assert!(pdc_write_allowed(new, offset));
    }
    // Pin 67's enable bank is word 2 (0x18), pin 33's is word 1 (0x14, the lid's too); only before 3.2.
    assert!(pdc_write_allowed(old, 0x18));
    assert!(!pdc_write_allowed(new, 0x18));
    for offset in [0x1c, 0x218, 0x220, 0x190, 0x198] {
        assert!(!pdc_write_allowed(old, offset), "{:#x}", offset);
        assert!(!pdc_write_allowed(new, offset), "{:#x}", offset);
    }
}

#[test]
fn the_pd_controller_pins_too() {
    // IRQ_i_CFG of pin 42 (GPIO 4, the adapter's port) and pin 50 (GPIO 3); their enable bank is word 1, as the lid's.
    for version in [0x30100, 0x30200] {
        assert!(pdc_write_allowed(version, 0x110 + 4 * 42));
        assert!(pdc_write_allowed(version, 0x110 + 4 * 50));
        assert!(!pdc_write_allowed(version, 0x110 + 4 * 41));
        assert!(!pdc_write_allowed(version, 0x110 + 4 * 51));
    }
}
