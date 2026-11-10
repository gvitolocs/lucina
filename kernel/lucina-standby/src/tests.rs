// SPDX-License-Identifier: MPL-2.0

use super::*;

const M: i64 = MINUTE_US;

/// The owner's defaults with Windows' battery screen-off.
const BATTERY: Times = Times { off: 5, sleep: SLEEP_DEFAULT_MIN, standby: STANDBY_DEFAULT_MIN };

/// Runs `decide` and `apply` at `now`, as the shell does when the deadline comes.
fn tick(s: Standby, now: i64, t: Times) -> (Standby, Decision) {
    let d = decide(s, now, t);
    (apply(s, d), d)
}

#[test]
fn off_at_5_asleep_at_15_standby_at_60() {
    let s = start(0);
    assert_eq!(deadline(s, BATTERY), Some(5 * M));
    assert_eq!(tick(s, 5 * M - 1, BATTERY).1, Decision::Stay);
    let (s, d) = tick(s, 5 * M, BATTERY);
    assert_eq!((d, s.screen), (Decision::ScreenOff, Screen::Dark));
    assert_eq!(deadline(s, BATTERY), Some(15 * M));
    assert_eq!(tick(s, 15 * M - 1, BATTERY).1, Decision::Stay);
    let (s, d) = tick(s, 15 * M, BATTERY);
    assert_eq!((d, s.screen), (Decision::Sleep, Screen::Asleep));
    assert_eq!(deadline(s, BATTERY), Some(60 * M));
    assert_eq!(tick(s, 60 * M - 1, BATTERY).1, Decision::Stay);
    let (s, d) = tick(s, 60 * M, BATTERY);
    assert_eq!((d, s.screen), (Decision::Standby, Screen::Standby));
    assert_eq!(deadline(s, BATTERY), None);
    assert_eq!(tick(s, 1000 * M, BATTERY).1, Decision::Stay);
}

#[test]
fn a_sleep_before_the_screen_off_sleeps_from_lit() {
    let t = Times { off: 30, sleep: 15, standby: 0 };
    let s = start(0);
    assert_eq!(deadline(s, t), Some(15 * M));
    assert_eq!(tick(s, 15 * M, t).1, Decision::Sleep);
    // The screen never on Never, the sleep still comes.
    let t = Times { off: 0, sleep: 15, standby: 0 };
    assert_eq!(tick(start(0), 15 * M, t).1, Decision::Sleep);
}

#[test]
fn never_means_never() {
    let t = Times { off: 0, sleep: 0, standby: 0 };
    let s = start(0);
    assert_eq!(deadline(s, t), None);
    assert_eq!(tick(s, 1000 * M, t).1, Decision::Stay);
    // Sleep Never: the screen goes off, nothing else; a lid sleep goes on to standby all the same.
    let t = Times { off: 5, sleep: 0, standby: 60 };
    let (dark, d) = tick(start(0), 5 * M, t);
    assert_eq!(d, Decision::ScreenOff);
    assert_eq!(deadline(dark, t), None);
    let lid = sleep(start(0));
    assert_eq!(tick(lid, 60 * M, t).1, Decision::Standby);
    // Standby Never: asleep stays asleep.
    let t = Times { off: 5, sleep: 15, standby: 0 };
    let asleep = sleep(start(0));
    assert_eq!(deadline(asleep, t), None);
    assert_eq!(tick(asleep, 1000 * M, t).1, Decision::Stay);
}

#[test]
fn input_wakes_from_anything_and_restarts() {
    for screen in [Screen::Lit, Screen::Dark, Screen::Asleep, Screen::Standby] {
        let s = Standby { screen, ..start(0) };
        let s = input(s, 30 * M);
        assert_eq!(s.screen, Screen::Lit);
        assert_eq!(deadline(s, BATTERY), Some(35 * M));
        assert_eq!(wake(Standby { screen, ..start(0) }, 30 * M), s);
    }
}

#[test]
fn a_display_hold_keeps_it_all_and_lights_a_dark_screen() {
    let (dark, _) = tick(start(0), 5 * M, BATTERY);
    let held = take(dark, Kind::Display);
    assert_eq!(held.screen, Screen::Lit);
    assert_eq!(deadline(held, BATTERY), None);
    assert_eq!(tick(held, 500 * M, BATTERY).1, Decision::Stay);
    let free = give(held, Kind::Display, 20 * M);
    assert_eq!(deadline(free, BATTERY), Some(25 * M));
    // Asleep or in standby, a display hold wakes nothing and goes nowhere.
    let asleep = take(sleep(start(0)), Kind::Display);
    assert_eq!(asleep.screen, Screen::Asleep);
    assert_eq!(tick(asleep, 500 * M, BATTERY).1, Decision::Stay);
}

#[test]
fn a_system_hold_lets_the_screen_go_off_but_not_sleep_or_standby() {
    let s = take(start(0), Kind::System);
    let (s, d) = tick(s, 5 * M, BATTERY);
    assert_eq!(d, Decision::ScreenOff);
    assert_eq!(deadline(s, BATTERY), None);
    assert_eq!(tick(s, 600 * M, BATTERY).1, Decision::Stay);
    // Audio stops at 40 min: the sleep is 15 min of idle after that, the standby 60.
    let s = give(s, Kind::System, 40 * M);
    assert_eq!(deadline(s, BATTERY), Some(55 * M));
    let (s, d) = tick(s, 55 * M, BATTERY);
    assert_eq!(d, Decision::Sleep);
    assert_eq!(deadline(s, BATTERY), Some(100 * M));
    // An agent's inhibitor taken while asleep keeps it from standby.
    let held = take(s, Kind::System);
    assert_eq!(held.screen, Screen::Asleep);
    assert_eq!(tick(held, 200 * M, BATTERY).1, Decision::Stay);
    let free = give(held, Kind::System, 70 * M);
    assert_eq!(deadline(free, BATTERY), Some(130 * M));
}

#[test]
fn only_the_last_give_ends_a_kind() {
    let s = take(take(start(0), Kind::System), Kind::System);
    let one = give(s, Kind::System, 10 * M);
    assert_eq!((one.system_holds, one.system_ended_us), (1, 0));
    let none = give(one, Kind::System, 12 * M);
    assert_eq!((none.system_holds, none.system_ended_us), (0, 12 * M));
    assert_eq!(give(none, Kind::System, 13 * M), none);
    assert_eq!(give(none, Kind::Display, 13 * M), none);
}

#[test]
fn the_lid_sleeps_whatever_is_held() {
    let s = sleep(take(take(start(0), Kind::Display), Kind::System));
    assert_eq!(s.screen, Screen::Asleep);
    assert_eq!(deadline(s, BATTERY), None);
    assert_eq!(wake(s, 3 * M).screen, Screen::Lit);
}

#[test]
fn a_refusal_tries_again_a_full_timeout_later() {
    let s = refused(start(0), 5 * M);
    assert_eq!(s.screen, Screen::Lit);
    assert_eq!(deadline(s, BATTERY), Some(10 * M));
}

#[test]
fn every_time_counts_from_the_last_activity() {
    let mut s = input(start(0), M);
    s = give(take(s, Kind::Display), Kind::Display, 3 * M);
    s = give(take(s, Kind::System), Kind::System, 4 * M);
    assert_eq!(screen_off_at(s, BATTERY), Some(8 * M));
    assert_eq!(sleep_at(s, BATTERY), Some(19 * M));
    assert_eq!(standby_at(s, BATTERY), Some(64 * M));
}

#[test]
fn the_settings_stay_in_bounds() {
    let d = Times { off: 5, sleep: SLEEP_DEFAULT_MIN, standby: STANDBY_DEFAULT_MIN };
    assert_eq!(settle(d), d);
    assert_eq!(settle(Times { off: 9000, sleep: 9000, standby: 9000 }), Times { off: 300, sleep: 300, standby: 300 });
    // A standby set before the sleep is moved to it; Never stays Never.
    assert_eq!(settle(Times { off: 5, sleep: 30, standby: 10 }), Times { off: 5, sleep: 30, standby: 30 });
    assert_eq!(settle(Times { off: 5, sleep: 30, standby: 0 }), Times { off: 5, sleep: 30, standby: 0 });
    assert_eq!(settle(Times { off: 5, sleep: 0, standby: 10 }), Times { off: 5, sleep: 0, standby: 10 });
    for t in [Times { off: 1, sleep: 2, standby: 1 }, Times { off: 300, sleep: 120, standby: 60 }] {
        assert_eq!(settle(settle(t)), settle(t));
    }
}
