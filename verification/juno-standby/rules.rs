// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes kernel/lucina-standby/src/lib.rs by
// #[path], so Verus checks `decide`, `deadline`, `settle`, `take`, `give` and the rest as the shell compiles them; each promises
// its `*_spec` in its `ensures`. The lemmas below state the properties the owner asked for over the fields themselves, not over
// those specs, so a bug planted in a function and its spec alike still fails here (verification/juno-standby/mutants.sh).
// Numbered as in lib.rs:
//   1. no sleep by itself while held or before `sleep` minutes of idle; standby only from a sleep, unheld, after `standby` minutes;
//   2. no screen-off while a display hold is live or before `off` minutes; a screen-off only when the sleep is not due;
//   3. Never is never;
//   4. the timers count from the latest input or hold end;
//   5. input lights from any state; a display hold lights a dark screen; no hold wakes a sleeping machine;
//   6. `deadline` is exact;
//   7. holds count;
//   8. the settings: bounded, a standby never before the sleep, the defaults kept.
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-standby/src/lib.rs"]
pub mod standby;

use standby::*;
use vstd::prelude::*;

verus! {

pub open spec fn times_ok(t: Times) -> bool {
    t.off <= MAX_MINUTES && t.sleep <= MAX_MINUTES && t.standby <= MAX_MINUTES
}

pub open spec fn ok(s: Standby, now: i64, t: Times) -> bool {
    bounded(s) && 0 <= now < MAX_TIME_US && times_ok(t)
}

/// 1. A sleep by itself.
proof fn sleeps_only_unheld_and_after_the_idle_minutes(s: Standby, now: i64, t: Times)
    requires
        ok(s, now, t),
        decide_spec(s, now, t) == Decision::Sleep,
    ensures
        s.display_holds == 0,
        s.system_holds == 0,
        s.screen == Screen::Lit || s.screen == Screen::Dark,
        t.sleep > 0,
        now >= s.last_input_us + t.sleep * MINUTE_US,
        now >= s.display_ended_us + t.sleep * MINUTE_US,
        now >= s.system_ended_us + t.sleep * MINUTE_US,
{
}

/// 1. The standby by itself: only from a sleep.
proof fn standby_only_from_a_sleep_unheld_and_late(s: Standby, now: i64, t: Times)
    requires
        ok(s, now, t),
        decide_spec(s, now, t) == Decision::Standby,
    ensures
        s.display_holds == 0,
        s.system_holds == 0,
        s.screen == Screen::Asleep,
        t.standby > 0,
        now >= s.last_input_us + t.standby * MINUTE_US,
        now >= s.display_ended_us + t.standby * MINUTE_US,
        now >= s.system_ended_us + t.standby * MINUTE_US,
{
}

/// 2. A screen-off by itself.
proof fn screen_off_only_unheld_late_and_before_the_sleep(s: Standby, now: i64, t: Times)
    requires
        ok(s, now, t),
        decide_spec(s, now, t) == Decision::ScreenOff,
    ensures
        s.display_holds == 0,
        s.screen == Screen::Lit,
        t.off > 0,
        now >= s.last_input_us + t.off * MINUTE_US,
        now >= s.display_ended_us + t.off * MINUTE_US,
        // the sleep is not due: held, Never, or not yet
        s.system_holds > 0 || t.sleep == 0 || now < s.last_input_us + t.sleep * MINUTE_US
            || now < s.display_ended_us + t.sleep * MINUTE_US || now < s.system_ended_us + t.sleep * MINUTE_US,
{
}

/// 3. Never is never; a display hold stops everything; standby is left to input.
proof fn never_is_never(s: Standby, now: i64, t: Times)
    requires
        ok(s, now, t),
    ensures
        t.sleep == 0 ==> decide_spec(s, now, t) != Decision::Sleep,
        t.standby == 0 ==> decide_spec(s, now, t) != Decision::Standby,
        t.off == 0 ==> decide_spec(s, now, t) != Decision::ScreenOff,
        s.display_holds > 0 ==> decide_spec(s, now, t) == Decision::Stay,
        s.screen == Screen::Standby ==> decide_spec(s, now, t) == Decision::Stay,
{
}

/// 4. After input at `now`, nothing happens before the first of the screen-off and the sleep.
proof fn input_restarts_the_timers(s: Standby, now: i64, tt: i64, t: Times)
    requires
        ok(s, now, t),
        bounded(input_spec(s, now)),
        now <= tt < MAX_TIME_US,
        t.off == 0 || tt < now + t.off * MINUTE_US,
        t.sleep == 0 || tt < now + t.sleep * MINUTE_US,
    ensures
        decide_spec(input_spec(s, now), tt, t) == Decision::Stay,
{
}

/// 4. After the last system hold ends at `now`, no sleep and no standby before their whole delay has passed again.
proof fn the_end_of_a_system_hold_restarts_the_sleep_and_standby(s: Standby, now: i64, tt: i64, t: Times)
    requires
        ok(s, now, t),
        s.system_holds == 1,
        now <= tt < MAX_TIME_US,
    ensures
        tt < now + t.sleep * MINUTE_US ==> decide_spec(give_spec(s, Kind::System, now), tt, t) != Decision::Sleep,
        tt < now + t.standby * MINUTE_US ==> decide_spec(give_spec(s, Kind::System, now), tt, t) != Decision::Standby,
{
}

/// 4. After the last display hold ends at `now`, nothing happens before the first of the screen-off and the sleep.
proof fn the_end_of_a_display_hold_restarts_the_timers(s: Standby, now: i64, tt: i64, t: Times)
    requires
        ok(s, now, t),
        s.display_holds == 1,
        s.screen == Screen::Lit || s.screen == Screen::Dark,
        now <= tt < MAX_TIME_US,
        t.off == 0 || tt < now + t.off * MINUTE_US,
        t.sleep == 0 || tt < now + t.sleep * MINUTE_US,
    ensures
        decide_spec(give_spec(s, Kind::Display, now), tt, t) == Decision::Stay,
{
}

/// 5. What lights and what does not.
proof fn input_lights_and_holds_do_not_wake(s: Standby, now: i64, k: Kind)
    ensures
        input_spec(s, now).screen == Screen::Lit,
        input_spec(s, now).last_input_us == now,
        s.screen == Screen::Dark ==> take_spec(s, Kind::Display).screen == Screen::Lit,
        s.screen == Screen::Asleep ==> take_spec(s, k).screen == Screen::Asleep,
        s.screen == Screen::Standby ==> take_spec(s, k).screen == Screen::Standby,
        s.screen == Screen::Lit ==> take_spec(s, k).screen == Screen::Lit,
        k == Kind::System ==> take_spec(s, k).screen == s.screen,
        give_spec(s, k, now).screen == s.screen,
{
}

/// 6. The deadline is exact.
proof fn the_deadline_is_exact(s: Standby, now: i64, t: Times)
    requires
        ok(s, now, t),
    ensures
        deadline_spec(s, t).is_none() ==> decide_spec(s, now, t) == Decision::Stay,
        deadline_spec(s, t).is_some() && (now as int) < (deadline_spec(s, t).unwrap() as int)
            ==> decide_spec(s, now, t) == Decision::Stay,
        deadline_spec(s, t).is_some() && (now as int) >= (deadline_spec(s, t).unwrap() as int)
            ==> decide_spec(s, now, t) != Decision::Stay,
{
}

/// 7. A take and its give.
proof fn a_take_and_its_give(s: Standby, k: Kind, now: i64)
    requires
        s.display_holds < MAX_HOLDS,
        s.system_holds < MAX_HOLDS,
    ensures
        give_spec(take_spec(s, k), k, now).display_holds == s.display_holds,
        give_spec(take_spec(s, k), k, now).system_holds == s.system_holds,
        give_spec(take_spec(s, k), k, now).last_input_us == s.last_input_us,
        k == Kind::Display && s.display_holds == 0 ==> give_spec(take_spec(s, k), k, now).display_ended_us == now,
        k == Kind::Display && s.display_holds > 0 ==> give_spec(take_spec(s, k), k, now).display_ended_us == s.display_ended_us,
        k == Kind::System && s.system_holds == 0 ==> give_spec(take_spec(s, k), k, now).system_ended_us == now,
        k == Kind::System && s.system_holds > 0 ==> give_spec(take_spec(s, k), k, now).system_ended_us == s.system_ended_us,
        k == Kind::Display ==> give_spec(take_spec(s, k), k, now).system_ended_us == s.system_ended_us,
        k == Kind::System ==> give_spec(take_spec(s, k), k, now).display_ended_us == s.display_ended_us,
{
}

/// 7. A give with none held is nothing; a give with more than one keeps the kind held.
proof fn gives_never_underflow_and_only_the_last_ends(s: Standby, k: Kind, now: i64)
    ensures
        k == Kind::Display && s.display_holds == 0 ==> give_spec(s, k, now) == s,
        k == Kind::System && s.system_holds == 0 ==> give_spec(s, k, now) == s,
        k == Kind::Display && s.display_holds > 1 ==> give_spec(s, k, now).display_holds > 0
            && give_spec(s, k, now).display_ended_us == s.display_ended_us,
        k == Kind::System && s.system_holds > 1 ==> give_spec(s, k, now).system_holds > 0
            && give_spec(s, k, now).system_ended_us == s.system_ended_us,
{
}

/// 8. The settings: in bounds, a standby never before the sleep, Never kept, values in bounds kept, settling twice the same.
proof fn the_settings_stay_in_bounds(t: Times)
    ensures
        settle_spec(t).off <= 300 && settle_spec(t).sleep <= 300 && settle_spec(t).standby <= 300,
        settle_spec(t).standby == 0 || settle_spec(t).sleep == 0 || settle_spec(t).standby >= settle_spec(t).sleep,
        t.sleep == 0 ==> settle_spec(t).sleep == 0,
        t.standby == 0 ==> settle_spec(t).standby == 0,
        t.off <= 300 ==> settle_spec(t).off == t.off,
        t.sleep <= 300 ==> settle_spec(t).sleep == t.sleep,
        t.off <= 300 && t.sleep <= 300 && t.standby <= 300 && (t.standby == 0 || t.sleep == 0 || t.standby >= t.sleep)
            ==> settle_spec(t) == t,
        settle_spec(settle_spec(t)) == settle_spec(t),
{
}

/// 8. The owner's defaults are settled already, and with them the standby comes after the sleep.
proof fn the_defaults_are_the_owners()
    ensures
        SLEEP_DEFAULT_MIN == 15 && STANDBY_DEFAULT_MIN == 60,
        settle_spec(Times { off: 5, sleep: SLEEP_DEFAULT_MIN, standby: STANDBY_DEFAULT_MIN })
            == (Times { off: 5, sleep: 15, standby: 60 }),
        settle_spec(Times { off: 10, sleep: SLEEP_DEFAULT_MIN, standby: STANDBY_DEFAULT_MIN })
            == (Times { off: 10, sleep: 15, standby: 60 }),
{
}

/// One turn of the shell: an event, then the decision at the same time, carried out.
pub enum Event {
    Input,
    PowerSleep,
    Take(Kind),
    Give(Kind),
    Nothing,
}

pub fn turn(s: Standby, e: Event, now: i64, t: Times) -> (r: (Standby, Decision))
    requires
        ok(s, now, t),
        s.display_holds < MAX_HOLDS,
        s.system_holds < MAX_HOLDS,
        s.last_input_us <= now,
        s.display_ended_us <= now,
        s.system_ended_us <= now,
    ensures
        bounded(r.0),
        r.1 == Decision::Sleep ==> r.0.screen == Screen::Asleep && r.0.display_holds == 0 && r.0.system_holds == 0,
        r.1 == Decision::Standby ==> r.0.screen == Screen::Standby && r.0.display_holds == 0 && r.0.system_holds == 0,
        // standby is never reached in one turn from a machine that was lit or dark
        (s.screen == Screen::Lit || s.screen == Screen::Dark) && !(e is PowerSleep) ==> r.0.screen != Screen::Standby,
        (e is Input) ==> r.0.screen == Screen::Lit,
{
    let s1 = match e {
        Event::Input => input(s, now),
        Event::PowerSleep => sleep(s),
        Event::Take(k) => take(s, k),
        Event::Give(k) => give(s, k, now),
        Event::Nothing => s,
    };
    let d = decide(s1, now, t);
    (apply(s1, d), d)
}

} // verus!

fn main() {}
