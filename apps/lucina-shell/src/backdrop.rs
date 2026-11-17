//! The background classes and the memory-pressure ladder, as pure rules (`docs/design/juno-window-manager.md` section 5).
//!
//! Two things live here, and nothing else: the level rule with its hysteresis (the shell's own copy of the kernel's, so the
//! shell can follow a level it is told as well as one it reads itself) and the ladder, a function from the apps that are
//! alive, the level and the memory figure to the list of things to do to them. No process is signalled and no file is
//! written here: the shell's UI thread applies the actions with `Running::set_paused` and `signal_group`
//! (`apps.rs:414`, `:425`) and the `oom_score_adj` writes of section 5's prerequisite 4.
//!
//! Bounded by construction: at most `MAX_APPS` of a longer input are looked at, and every sum and product is saturating or
//! clamped, so any input gives a Vec and never a panic. The only allocation is the returned Vec.
//!
//! Where this is the shell's copy and the kernel is the authority: `lucina-mempressure` (the Verus crate section 5 asks
//! for, on branch `mempressure3` and not in this tree) holds the same rule over KiB with a `Config`; the thresholds below
//! are its defaults, in MiB. The ranking function section 5 puts in `lucina-wm-rules` is here rather than there because
//! that crate is under proof (`docs/ci/PROOF-COVERAGE.md` section 3) and a new unit in it needs the proof workflow.

#![allow(dead_code)] // slice W: the shell still runs one app on screen and reads memory on a timer, so nothing calls this yet.

/// Available memory that drops the level to `low` (section 5: enter `low` below 600 MiB).
pub const LOW_ENTER_MIB: u64 = 600;
/// Available memory that lifts it out of `low` again (at or above 700 MiB).
pub const LOW_LEAVE_MIB: u64 = 700;
/// Available memory that drops the level to `critical` (below 250 MiB).
pub const CRITICAL_ENTER_MIB: u64 = 250;
/// Available memory that lifts it out of `critical` again (at or above 350 MiB).
pub const CRITICAL_LEAVE_MIB: u64 = 350;

/// The most apps the ladder plans for. A longer input is read as its first `MAX_APPS` entries and the tail is ignored: the
/// caller passes them most-recently-used first, so what falls off is the coldest end, which is what the ladder would have
/// acted on last anyway. 16 is the cap the switcher's MRU list already holds.
pub const MAX_APPS: usize = 16;

/// The floor of the warm ramp, the step per rank, and the ceiling of the file (section 5, and `/proc/<pid>/oom_score_adj`).
const OOM_WARM_BASE: i32 = 100;
const OOM_WARM_STEP: i32 = 100;
const OOM_SCORE_MAX: i32 = 1000;

/// Where an app is on screen, which is what decides what the ladder may do to it (section 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The focused app. Nothing here ever touches it.
    Front,
    /// Shown as a tile or mid-swipe: it runs.
    Visible,
    /// Off screen: frozen under pressure, and the first to be closed.
    Warm,
    /// Closed after it saved its state. Its space keeps the last thumbnail; there is no process left to act on.
    Cold,
}

/// How tight memory is. Ordered: `Normal` is the least pressure, `Critical` the worst.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    Low,
    Critical,
}

impl Level {
    /// The level after one reading, with the hysteresis of section 5: a level is entered below its enter point and left
    /// only at or above its leave point, so a reading that sits in the band between the two keeps the level it has and
    /// nothing flaps. The enter points move with `lucina.mem.low=` and `lucina.mem.critical=` on the kernel command line;
    /// this copy knows the defaults only.
    pub fn next(self, available_mib: u64) -> Level {
        match self {
            // A drop from `normal` runs past `low` straight into `critical` in one step, as the kernel rule does.
            Level::Normal => {
                if available_mib < CRITICAL_ENTER_MIB {
                    Level::Critical
                } else if available_mib < LOW_ENTER_MIB {
                    Level::Low
                } else {
                    Level::Normal
                }
            }
            Level::Low => {
                if available_mib < CRITICAL_ENTER_MIB {
                    Level::Critical
                } else if available_mib >= LOW_LEAVE_MIB {
                    Level::Normal
                } else {
                    Level::Low
                }
            }
            Level::Critical => {
                if available_mib >= LOW_LEAVE_MIB {
                    Level::Normal
                } else if available_mib >= CRITICAL_LEAVE_MIB {
                    Level::Low
                } else {
                    Level::Critical
                }
            }
        }
    }

    /// How much available memory the ladder works back towards at this level. Section 5 closes warm apps at `critical`
    /// "until above `low` again", which is `low_leave`; at `low` it works back to the enter point only.
    pub fn target_mib(self) -> u64 {
        match self {
            Level::Normal => 0,
            Level::Low => LOW_ENTER_MIB,
            Level::Critical => LOW_LEAVE_MIB,
        }
    }
}

/// What the ladder has to work with: the memory figure it is there to fix, and the headroom the caller wants on top of the
/// level's own target (the size of the app that is about to start, say).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    /// Available memory now, MiB: a `mem.pressure` event's `available_kib / 1024`, or `system::memory()` (`system.rs:165`).
    pub available_mib: u64,
    /// Extra room to leave behind it, MiB.
    pub spare_mib: u64,
}

impl Budget {
    /// How much more has to come free before the ladder may stop.
    fn need_mib(self, level: Level) -> u64 {
        level
            .target_mib()
            .saturating_add(self.spare_mib)
            .saturating_sub(self.available_mib)
    }
}

/// One app, as far as the ladder is concerned. The caller reads it from its own state on every call, so nothing here can
/// go stale between two pressure edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppState {
    /// The shell's handle for the app (its slot in the running list, or its pid), handed back unchanged by every action.
    pub id: u32,
    pub class: Class,
    /// Last focus, most recent first: 0 is the app the user touched last, the largest rank the coldest.
    pub mru_rank: u32,
    /// The app's own resident memory, MiB (`/proc/<pid>/status` VmRSS).
    pub mem_mib: u32,
    /// It answered `background` and its state is on disk. Only then is it frozen, and only then is it closed (section 5).
    pub state_saved: bool,
}

/// What the shell is told to do to one app. Every action carries the id it is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// SIGSTOP the group: `Running::set_paused(true)`.
    Freeze(u32),
    /// SIGCONT the group: `Running::set_paused(false)`.
    Thaw(u32),
    /// `background` (SIGUSR1): save your state and drop your caches. The app answers with `state-saved`, which sets
    /// `AppState::state_saved` and lets the next plan freeze or close it.
    AskSaveState(u32),
    /// Close it: SIGTERM, then SIGKILL after the grace (`apps::GRACE`). It becomes `cold`.
    Kill(u32),
    /// Write `oom_score_adj` by rank (section 5's prerequisite 4: the file exists, the shell does not write it yet).
    SetOomScore(u32, i32),
}

/// The `oom_score_adj` of a class: the front and whatever is on screen are lowest, and within `warm` the least recently
/// used is highest (100 + 100 per rank, section 5), saturating at the file's ceiling.
pub fn oom_score_adj(class: Class, mru_rank: u32) -> i32 {
    // i64, so that any rank at all fits the product; the result is clamped into the range the file takes.
    let ramp = i64::from(OOM_WARM_BASE) + i64::from(OOM_WARM_STEP) * i64::from(mru_rank);
    match class {
        // The front and the tiles are worth as much to the user as the shell itself, which sits at -1000.
        Class::Front | Class::Visible => 0,
        Class::Warm => ramp.clamp(0, i64::from(OOM_SCORE_MAX)) as i32,
        // No process of its own. If one came back without the ladder's say, it is the coldest thing there is.
        Class::Cold => OOM_SCORE_MAX,
    }
}

/// What to do to the live apps at `level`, in the order the shell should do it:
///
/// 1. the `oom_score_adj` writes, so the kernel's killer holds the ranks before anything is stopped or closed;
/// 2. at `critical`, the closes, coldest first, until the budget fits — never the front, never the last visible app;
/// 3. then, at `low` and `critical`, the off-screen apps that are left: frozen once they saved their state, asked to save
///    it while they have not (nothing is stopped before it answered `background`, section 5);
/// 4. at `normal`, the thaw of everything the ladder stopped.
///
/// A `cold` app gets nothing: it is closed already, which is what `Kill` turns a warm app into. The front gets nothing at
/// any level. A longer input is read as its first `MAX_APPS` entries (`MAX_APPS` says why); the same input always gives the
/// same list in the same order.
pub fn plan(apps: &[AppState], level: Level, budget: Budget) -> Vec<Action> {
    let apps = &apps[..apps.len().min(MAX_APPS)];
    let mut out = Vec::with_capacity(apps.len() * 2);
    for a in apps {
        if a.class == Class::Warm {
            out.push(Action::SetOomScore(
                a.id,
                oom_score_adj(a.class, a.mru_rank),
            ));
        }
    }
    match level {
        Level::Normal => {
            for a in apps {
                if a.class == Class::Warm {
                    out.push(Action::Thaw(a.id));
                }
            }
        }
        Level::Low => {
            // Section 5 also closes the least recently used warm app while available *stays* below `low`. "Stays" is a
            // timer the shell does not have yet (slice W), so at `low` the ladder prepares the close and stops the app;
            // closing on a number starts at `critical`.
            freeze_rest(apps, &[false; MAX_APPS], &mut out);
        }
        Level::Critical => {
            let mut closed = [false; MAX_APPS];
            close_coldest_first(apps, budget.need_mib(level), &mut closed, &mut out);
            freeze_rest(apps, &closed, &mut out);
        }
    }
    out
}

/// Freeze or ask the warm apps that are still running. `closed` is what step 2 of `plan` ended, by the same index.
fn freeze_rest(apps: &[AppState], closed: &[bool; MAX_APPS], out: &mut Vec<Action>) {
    for (i, a) in apps.iter().enumerate() {
        if a.class != Class::Warm || closed[i] {
            continue;
        }
        out.push(if a.state_saved {
            Action::Freeze(a.id)
        } else {
            Action::AskSaveState(a.id)
        });
    }
}

/// Close apps, the coldest first, until `need_mib` of their memory is free or nothing may be closed. What it closed is
/// marked in `closed`, so step 3 of `plan` does not freeze a corpse.
fn close_coldest_first(
    apps: &[AppState],
    need_mib: u64,
    closed: &mut [bool; MAX_APPS],
    out: &mut Vec<Action>,
) {
    let visible = apps.iter().filter(|a| a.class == Class::Visible).count();
    let mut killed_visible = 0;
    let mut freed: u64 = 0;
    while freed < need_mib {
        let Some(i) = coldest(apps, closed, killed_visible, visible) else {
            break;
        };
        closed[i] = true;
        if apps[i].class == Class::Visible {
            // One less thing on screen, counted at once: the next pick must respect it.
            killed_visible += 1;
        }
        freed = freed.saturating_add(u64::from(apps[i].mem_mib));
        out.push(Action::Kill(apps[i].id));
    }
}

/// The index of the coldest app that may be closed: the largest `mru_rank`, the bigger of two equal ranks first, the
/// earlier of two equal entries kept. Never the front, never a cold app (it has no process), never the last visible one.
fn coldest(
    apps: &[AppState],
    closed: &[bool; MAX_APPS],
    killed_visible: usize,
    visible: usize,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, a) in apps.iter().enumerate() {
        if closed[i] {
            continue;
        }
        let may = match a.class {
            Class::Front | Class::Cold => false,
            // All but one of the apps on screen may go: an overview with nothing in it is not an overview.
            Class::Visible => killed_visible + 1 < visible,
            Class::Warm => true,
        };
        if !may {
            continue;
        }
        let colder = match best {
            None => true,
            Some(j) => {
                let b = &apps[j];
                a.mru_rank > b.mru_rank || (a.mru_rank == b.mru_rank && a.mem_mib > b.mem_mib)
            }
        };
        if colder {
            best = Some(i);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: u32, class: Class, mru_rank: u32, mem_mib: u32, state_saved: bool) -> AppState {
        AppState {
            id,
            class,
            mru_rank,
            mem_mib,
            state_saved,
        }
    }

    /// An off-screen app that has saved its state, so the ladder may stop it or close it.
    fn warm(id: u32, mru_rank: u32, mem_mib: u32) -> AppState {
        app(id, Class::Warm, mru_rank, mem_mib, true)
    }

    fn budget(available_mib: u64) -> Budget {
        Budget {
            available_mib,
            spare_mib: 0,
        }
    }

    /// The id an action is about, whatever it is.
    fn target(a: &Action) -> u32 {
        match a {
            Action::Freeze(i)
            | Action::Thaw(i)
            | Action::AskSaveState(i)
            | Action::Kill(i)
            | Action::SetOomScore(i, _) => *i,
        }
    }

    fn has(out: &[Action], want: Action) -> bool {
        out.contains(&want)
    }

    fn kills(out: &[Action]) -> Vec<u32> {
        out.iter()
            .filter_map(|a| match a {
                Action::Kill(i) => Some(*i),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_low_band_never_flaps() {
        // Entered below 600, left at 700: everything between the two keeps the level it had.
        assert_eq!(Level::Normal.next(599), Level::Low);
        assert_eq!(Level::Normal.next(600), Level::Normal);
        for mib in 600..700 {
            assert_eq!(
                Level::Low.next(mib),
                Level::Low,
                "{mib} MiB sits in the band and must not move the level"
            );
        }
        assert_eq!(Level::Low.next(700), Level::Normal);
        // No flapping: a reading that swings about inside the band holds the level once it is in it. 650 is above the
        // enter point, so it does not bring `low` on its own; what it cannot do is take a `low` away again.
        assert_eq!(Level::Normal.next(650), Level::Normal);
        let mut level = Level::Normal.next(599);
        assert_eq!(level, Level::Low);
        for mib in [650, 601, 699, 650, 600, 599] {
            level = level.next(mib);
            assert_eq!(
                level,
                Level::Low,
                "{mib} MiB sits in the band and must not move the level"
            );
        }
    }

    #[test]
    fn critical_is_entered_in_one_step_and_left_through_low() {
        assert_eq!(Level::Normal.next(249), Level::Critical);
        assert_eq!(Level::Normal.next(250), Level::Low);
        assert_eq!(Level::Low.next(249), Level::Critical);
        assert_eq!(Level::Low.next(250), Level::Low);
        assert_eq!(Level::Critical.next(349), Level::Critical);
        assert_eq!(Level::Critical.next(350), Level::Low);
        // Only 700 clears low, so a climb out of critical lands on low first.
        assert_eq!(Level::Critical.next(699), Level::Low);
        assert_eq!(Level::Critical.next(700), Level::Normal);
    }

    #[test]
    fn the_level_is_monotone_in_available() {
        // Less available never gives a lower level than more, from the same previous level.
        for prev in [Level::Normal, Level::Low, Level::Critical] {
            let mut last = Level::Normal;
            for mib in (0..=1200u64).rev() {
                let now = prev.next(mib);
                assert!(
                    now >= last,
                    "{prev:?} at {mib} MiB gave {now:?}, below the {last:?} of more memory"
                );
                last = now;
            }
        }
    }

    #[test]
    fn critical_is_never_reported_at_or_above_its_leave_point() {
        for prev in [Level::Normal, Level::Low, Level::Critical] {
            for mib in CRITICAL_LEAVE_MIB..=4000 {
                assert_ne!(prev.next(mib), Level::Critical, "{prev:?} at {mib} MiB");
            }
        }
    }

    #[test]
    fn the_front_is_never_touched_at_any_level() {
        for level in [Level::Normal, Level::Low, Level::Critical] {
            let apps = [
                app(1, Class::Front, 0, 900, false),
                warm(2, 3, 400),
                app(3, Class::Visible, 1, 300, false),
            ];
            let out = plan(&apps, level, budget(10));
            for a in &out {
                assert_ne!(target(a), 1, "{level:?} touched the front with {a:?}");
            }
        }
    }

    #[test]
    fn low_freezes_a_warm_app_that_saved_and_asks_one_that_did_not() {
        let apps = [warm(1, 0, 100), app(2, Class::Warm, 1, 100, false)];
        let out = plan(&apps, Level::Low, budget(300));
        assert!(
            has(&out, Action::Freeze(1)),
            "saved and off screen: stopped at {out:?}"
        );
        assert!(
            has(&out, Action::AskSaveState(2)),
            "not saved yet: asked first at {out:?}"
        );
        assert!(
            !has(&out, Action::Freeze(2)),
            "nothing is stopped before it answered"
        );
        assert!(kills(&out).is_empty(), "low closes nothing: {out:?}");
    }

    #[test]
    fn low_and_normal_leave_the_visible_and_the_cold_alone() {
        let apps = [
            app(1, Class::Front, 0, 100, false),
            app(2, Class::Visible, 1, 100, false),
            app(3, Class::Cold, 2, 100, true),
        ];
        for level in [Level::Normal, Level::Low] {
            assert_eq!(plan(&apps, level, budget(10)), Vec::new(), "{level:?}");
        }
    }

    #[test]
    fn a_cold_app_gets_nothing_at_any_level() {
        let apps = [app(1, Class::Cold, 5, 4000, true)];
        for level in [Level::Normal, Level::Low, Level::Critical] {
            assert_eq!(plan(&apps, level, budget(1)), Vec::new(), "{level:?}");
        }
    }

    #[test]
    fn critical_closes_coldest_first_until_the_budget_fits() {
        // Rank 0 is the most recent, so the coldest is the app with rank 3 and the order must run backwards from it.
        let apps = [
            app(1, Class::Front, 0, 100, false),
            warm(2, 1, 120),
            warm(3, 2, 130),
            warm(4, 3, 140),
        ];
        // Critical wants 700 available; at 560 MiB 140 must come free, which is exactly the coldest app.
        assert_eq!(kills(&plan(&apps, Level::Critical, budget(560))), vec![4]);
        // At 300 the whole 390 MiB of warm memory is not enough: all three go, in order, and the front stays.
        assert_eq!(
            kills(&plan(&apps, Level::Critical, budget(300))),
            vec![4, 3, 2]
        );
        // Already above the target: nothing is closed, the warm ones are only stopped.
        assert!(kills(&plan(&apps, Level::Critical, budget(900))).is_empty());
    }

    #[test]
    fn a_bigger_app_of_the_same_rank_closes_first() {
        let apps = [warm(1, 4, 80), warm(2, 4, 400)];
        let out = plan(&apps, Level::Critical, budget(400));
        assert_eq!(
            kills(&out),
            vec![2],
            "an equal rank goes to the one that pays most"
        );
        assert!(
            has(&out, Action::Freeze(1)),
            "the cheaper one is kept warm: {out:?}"
        );
    }

    #[test]
    fn critical_never_closes_the_last_visible_app() {
        // One tile on screen and nothing off it: there is no app the ladder may close at all.
        let apps = [
            app(1, Class::Front, 0, 100, false),
            app(2, Class::Visible, 1, 500, false),
        ];
        assert_eq!(plan(&apps, Level::Critical, budget(0)), Vec::new());
        // Two tiles: the coldest may go, the last one never, however tight it gets.
        let apps = [
            app(1, Class::Front, 0, 100, false),
            app(2, Class::Visible, 1, 500, false),
            app(3, Class::Visible, 2, 500, false),
        ];
        assert_eq!(
            kills(&plan(&apps, Level::Critical, budget(0))),
            vec![3],
            "the coldest tile goes and the last one stays"
        );
    }

    #[test]
    fn recovery_thaws_what_the_ladder_froze() {
        let apps = [
            app(1, Class::Front, 0, 100, false),
            warm(2, 1, 100),
            app(3, Class::Warm, 2, 100, false),
            app(4, Class::Visible, 3, 100, false),
        ];
        let out = plan(&apps, Level::Normal, budget(3000));
        assert!(has(&out, Action::Thaw(2)), "{out:?}");
        assert!(
            has(&out, Action::Thaw(3)),
            "thawed whether or not it had saved: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|a| matches!(a, Action::Thaw(1) | Action::Thaw(4)))
        );
        assert!(
            !out.iter()
                .any(|a| matches!(a, Action::Freeze(_) | Action::Kill(_))),
            "a recovery plan stops and closes nothing: {out:?}"
        );
    }

    #[test]
    fn the_oom_ramp_ranks_by_class_then_by_recency() {
        assert_eq!(oom_score_adj(Class::Front, 0), 0);
        assert_eq!(oom_score_adj(Class::Visible, 9), 0);
        assert_eq!(oom_score_adj(Class::Warm, 0), 100);
        assert_eq!(oom_score_adj(Class::Warm, 1), 200);
        assert_eq!(oom_score_adj(Class::Warm, 5), 600);
        assert_eq!(oom_score_adj(Class::Cold, 0), OOM_SCORE_MAX);
        // Monotone in rank, and every warm score above the front's and the visible ones'.
        let mut last = -1;
        for rank in 0..40 {
            let score = oom_score_adj(Class::Warm, rank);
            assert!(
                score >= last,
                "rank {rank} scored {score}, below the colder {last}"
            );
            assert!(score > oom_score_adj(Class::Front, rank));
            assert!(score > oom_score_adj(Class::Visible, rank));
            last = score;
        }
    }

    #[test]
    fn a_huge_rank_saturates_at_the_ceiling_and_never_overflows() {
        assert_eq!(oom_score_adj(Class::Warm, 9), 1000);
        assert_eq!(oom_score_adj(Class::Warm, u32::MAX), 1000);
        let apps = [warm(1, u32::MAX, u32::MAX)];
        let out = plan(&apps, Level::Low, budget(0));
        assert!(has(&out, Action::SetOomScore(1, 1000)), "{out:?}");
    }

    #[test]
    fn the_scores_come_before_anything_that_stops_or_closes() {
        let apps = [warm(1, 0, 100), warm(2, 1, 100)];
        let out = plan(&apps, Level::Critical, budget(600));
        let first_move = out
            .iter()
            .position(|a| !matches!(a, Action::SetOomScore(_, _)))
            .expect("critical with a deficit does something");
        assert!(
            out[..first_move]
                .iter()
                .all(|a| matches!(a, Action::SetOomScore(_, _))),
            "the killer must hold the ranks before an app is touched: {out:?}"
        );
        assert_eq!(out[first_move], Action::Kill(2), "coldest first: {out:?}");
        assert!(
            has(&out, Action::Freeze(1)),
            "and the one that is left is stopped: {out:?}"
        );
    }

    #[test]
    fn seventeen_apps_are_read_as_the_first_sixteen() {
        let apps: Vec<AppState> = (0..17).map(|i| warm(i, i, 10)).collect();
        let out = plan(&apps, Level::Low, budget(10));
        let frozen = out
            .iter()
            .filter(|a| matches!(a, Action::Freeze(_)))
            .count();
        assert_eq!(
            frozen, MAX_APPS,
            "the 17th is past the cap: ignored, not planned for"
        );
        assert!(
            !out.iter().any(|a| target(a) == 16),
            "id 16 is the 17th entry and must not appear"
        );
        assert_eq!(
            out.iter()
                .filter(|a| matches!(a, Action::SetOomScore(_, _)))
                .count(),
            frozen,
            "every app looked at is scored and stopped, and nothing else"
        );
    }

    #[test]
    fn an_empty_input_and_a_front_only_input_give_nothing() {
        assert_eq!(plan(&[], Level::Critical, budget(0)), Vec::new());
        assert_eq!(plan(&[], Level::Normal, Budget::default()), Vec::new());
        // Nothing but the front, at critical, with no memory at all: the ladder stops rather than reach for it.
        let apps = [app(1, Class::Front, 0, 100, false)];
        assert_eq!(plan(&apps, Level::Critical, budget(0)), Vec::new());
    }

    #[test]
    fn the_same_input_always_gives_the_same_plan() {
        let apps = [
            app(1, Class::Front, 0, 100, false),
            warm(2, 1, 130),
            app(3, Class::Warm, 2, 90, false),
            app(4, Class::Visible, 3, 400, false),
            app(5, Class::Visible, 4, 70, false),
            app(6, Class::Cold, 5, 20, true),
        ];
        for level in [Level::Normal, Level::Low, Level::Critical] {
            for available in [0u64, 240, 400, 690, 5000] {
                let b = Budget {
                    available_mib: available,
                    spare_mib: available % 300,
                };
                assert_eq!(
                    plan(&apps, level, b),
                    plan(&apps, level, b),
                    "{level:?} at {available}"
                );
            }
        }
    }

    #[test]
    fn no_input_at_all_produces_a_bounded_plan() {
        // Every shape a caller can hand over, through every level: no panic, and a plan no longer than a capped input can carry.
        let classes = [Class::Front, Class::Visible, Class::Warm, Class::Cold];
        let mut seed: u32 = 0x1234_5678;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            seed >> 8
        };
        for _ in 0..200 {
            let apps: Vec<AppState> = (0..(rnd() as usize) % 40)
                .map(|i| AppState {
                    id: i as u32,
                    class: classes[(rnd() as usize) % classes.len()],
                    mru_rank: rnd(),
                    mem_mib: rnd(),
                    state_saved: rnd() % 2 == 0,
                })
                .collect();
            for level in [Level::Normal, Level::Low, Level::Critical] {
                let b = Budget {
                    available_mib: u64::from(rnd()),
                    spare_mib: u64::from(rnd()),
                };
                let out = plan(&apps, level, b);
                assert!(
                    out.len() <= MAX_APPS * 3,
                    "{level:?} over {} apps gave {} actions, more than a capped input carries",
                    apps.len(),
                    out.len()
                );
            }
        }
    }

    #[test]
    fn closing_stops_at_the_target_and_the_headroom_is_paid_for_too() {
        // 600 available at critical: 100 MiB must come free (target 700), which is one 120 MiB app.
        let apps = [warm(1, 3, 120), warm(2, 2, 120), warm(3, 1, 120)];
        let out = plan(&apps, Level::Critical, budget(600));
        assert_eq!(kills(&out), vec![1], "one close is enough: {out:?}");
        assert!(
            has(&out, Action::Freeze(2)) && has(&out, Action::Freeze(3)),
            "{out:?}"
        );
        // The caller wants 120 MiB of headroom as well: that is a second app, and no third.
        let out = plan(
            &apps,
            Level::Critical,
            Budget {
                available_mib: 600,
                spare_mib: 120,
            },
        );
        assert_eq!(
            kills(&out),
            vec![1, 2],
            "the app about to start is paid for: {out:?}"
        );
        assert!(has(&out, Action::Freeze(3)), "{out:?}");
    }

    #[test]
    fn the_target_of_a_level_is_the_design_number() {
        assert_eq!(Level::Normal.target_mib(), 0);
        assert_eq!(Level::Low.target_mib(), 600);
        assert_eq!(Level::Critical.target_mib(), 700);
        assert_eq!(Level::Critical.target_mib(), LOW_LEAVE_MIB);
        assert_eq!(
            Budget::default(),
            Budget {
                available_mib: 0,
                spare_mib: 0
            }
        );
    }
}
