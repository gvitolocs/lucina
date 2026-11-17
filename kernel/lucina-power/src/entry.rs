// SPDX-License-Identifier: MPL-2.0

//! Which steps the way back runs, and in what order, for where an attempt stopped on the way in (patch 1365, with the
//! split contract v3 signs in patch 1367).
//!
//! The originals are Linux v6.12, `kernel/power/suspend.c` and `drivers/base/syscore.c`; the order they add up to is
//! §2 of `docs/design/juno-suspend-contract-v3.md`, and every step of the tables below is one of its lines.
//!
//! Linux unwinds the entry with error gotos to labels that fall through downwards (`suspend.c:466-481`, and the
//! caller's `:520-535`), so what a refusal costs is decided by which label it lands on. From the top of that stack:
//! `Close` (`:528-529`), where a `platform_suspend_begin` refusal goes (`:501-503`) and nothing of the resume list
//! runs; `Recover_platform` (`:533-535`), where a `dpm_suspend_start` one goes (`:507-511`) and which is answered with
//! the caller's `Resume_devices` and its `dpm_resume_end` (`:520-522`); `Platform_finish` (`:479-480`), from
//! `platform_suspend_prepare` (`:407`, the helper at `:257-260`) or `dpm_suspend_late` (`:411-415`);
//! `Devices_early_resume` (`:476-477`), from `platform_suspend_prepare_late` (`:416-418`); `Platform_early_resume`
//! (`:473-474`), from `dpm_suspend_noirq` (`:420-424`); `Platform_wake` (`:469-471`), from
//! `platform_suspend_prepare_noirq` (`:425-427`) or `suspend_test(TEST_PLATFORM)` (`:429-430`); `Enable_cpus`
//! (`:466-467`), from a `pm_sleep_disable_secondary_cpus` refusal (`:437-439`); and, below all of them, the way out of
//! a `syscore_suspend` that ran: `syscore_resume` (`:458`, under the `if (!error)` guard of `:447`), then
//! `system_state = SYSTEM_RUNNING` (`:461`), `arch_suspend_enable_irqs()` (`:463`) and
//! `pm_sleep_enable_secondary_cpus()` (`:467`).
//!
//! One step of the way back hangs on that fall-through: putting the CPUs and the interrupts back (`:461-467`) runs
//! only for an attempt that reached `Enable_cpus` (`:466-467`): a `pm_sleep_disable_secondary_cpus` refusal (`:437-439`,
//! which jumps straight to it), the two pending-wake asks around the syscore points and the wake of a sleep that was
//! taken. A refusal above `Platform_wake` (`:469`) never gets there - `Enable_cpus` sits above the label it jumps to -
//! so it runs no `resume_cpus`, and `Platform_wake` itself runs the platform's `wake` (`:470`) without it. That is what
//! the split of the stops below says apart, refusal by refusal.
//!
//! Where this port's rows are NOT the Linux label's whole list. Each row below is the steps of this vocabulary that
//! the label runs; it is not a claim that every step the label runs is implemented here. Limits for the first
//! platform implementer (nothing registers a `PlatformOps` on this machine, so none is exercised today):
//!
//! - `PlatformOps` has no hook for `platform_recover` (`:534`), `platform_resume_early` (`:474`) or
//!   `platform_resume_finish` (`:480`), so the rows that land on those labels run the devices alone; and it has no
//!   counterpart of `platform_suspend_prepare_late`'s own label (`Devices_early_resume`, `:416-418`, `:476-477`).
//! - The walk here is one device list with `Sequence` marks deciding which callbacks a `Devices` step resumes; it has
//!   no separate late phase. `PlatformOps::prepare` runs after the whole device walk, the noirq phase included, and
//!   after `can_commit`, where Linux runs `suspend_ops->prepare` at `:407`, before `dpm_suspend_late` (`:411`) and
//!   `dpm_suspend_noirq` (`:420`). So a `prepare` refusal here resumes devices that were already in noirq, which
//!   Linux's `Platform_finish` (`:479`) never has to. This order is inherited from patches 1341 and 1365.
//! - The pending-wake asks run after the CPUs are parked (`PlatformOps::park_cpus`, `:437`) and the interrupts are
//!   off (`:441`): the framework asks `park_cpus` to do both, and cannot check that it did.
//!
//! The two syscore asks are the rest of §2. The first is inside `syscore_suspend`, before its first callback, which a
//! pending wake refuses (`syscore.c:55-57`); the `if (!error)` guard (`suspend.c:447`) then skips `syscore_resume`
//! (`:458`) - no syscore suspend completed, so none is given back - and the way out is `:461-467`, `:470`, `:471`. The
//! second ask is right after the callbacks (`:448`); it answers `-EBUSY` without reaching `suspend_ops->enter`
//! (`:455-456`), and `syscore_resume` runs (`:458`) before the system is put back to running (`:461-467`) and the
//! platform's own wake (`:470`, the helper at `:278-286`), and only then the devices (`:471`, `dpm_resume_noirq`
//! `main.c:753`).
//!
//! A sleep that was taken ends on that same list: `:458`, `:461-467`, `:470` and `:471` run in that order after the
//! entry of `:452`. On the machines we have that is not a return from `suspend_ops->enter` but the resume entry that
//! continues the same attempt, which is what §2's step 3 - the entry, "which returns with IRQs still off" - means
//! here. Contract v3 §2 decides the order, and reverses 1341's `leave()`:
//! v2 had one `PlatformOps::wake` for the two Linux steps `:461-467` and `:470` and ran it before `syscore_resume`,
//! which is the deviation §1's split removes. The exceptions to "after the syscore resume" are the first of the two
//! asks, where no syscore resume ran at all and `resume_cpus` is the first step owed (`:447`, `:461-467`), and the park
//! refusal (`:437-439`), which jumps to `Enable_cpus` (`:466-467`) the same way.
//!
//! The s2idle path is not on that list. Linux's `suspend_enter` calls `s2idle_loop()` and jumps straight to
//! `Platform_wake` (`:432-434`), past `syscore_suspend` (`:446`), so a s2idle entry runs no syscore ops and owes no
//! syscore resume (contract v3 §3); its step back is `s2idle_ops->restore_early`, which is what `platform_resume_noirq`
//! calls for that state (`:280-282`, at `:470`), and then the devices (`:471`). Its two pending-wake asks are the
//! loop's before each entry (`:139-145`, the `pm_wakeup_pending` at `:143`) and `s2idle_enter`'s own (`:90`, the ask at
//! `:95`), and both land on `Platform_wake`.

/// Where the attempt stands, which is what decides what the way back owes. The variants that name a refusal are the
/// Linux label that refusal lands on, so that a row can be read off the file it comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop {
    /// Neither ask found a wake: the entry is taken (`suspend.c:452`, or the s2idle loop's call of `s2idle_enter` at
    /// `:150`) and nothing here is given back.
    Enter,
    /// `PlatformOps::begin` refused, which is Linux's `platform_suspend_begin` failing at `suspend.c:501-503`: the
    /// jump is to `Close` (`:528-529`), and none of the resume list runs.
    Begin,
    /// A device refused the way down or the veto ask that follows it: Linux's `dpm_suspend_start` failing
    /// (`:507-511`, `Recover_platform` `:533-535`) and its `dpm_suspend_noirq` failing (`:420-424`,
    /// `Platform_early_resume` `:473-474`), both answered by the caller's `Resume_devices` and `dpm_resume_end`
    /// (`:520-522`).
    DevicesDown,
    /// `PlatformOps::prepare` refused: Linux's `platform_suspend_prepare` failing at `suspend.c:407-409` (the helper
    /// at `:257-260`), or `dpm_suspend_late` failing at `:411-415`. Both go to `Platform_finish` (`:479-480`).
    PlatformPrepare,
    /// The last veto refused - Linux's `platform_suspend_prepare_noirq` at `suspend.c:425-427`, or
    /// `suspend_test(TEST_PLATFORM)` at `:429-430` - which goes to `Platform_wake` (`:469-471`). It is also where the
    /// s2idle path's two asks leave the attempt: the loop's (`:143`) breaks on it and `s2idle_enter`'s (`:95`) goes
    /// `out` (`:96`) and is answered by the loop on its next turn, and both reach the same label (`:434`).
    PlatformWake,
    /// `PlatformOps::park_cpus` refused: Linux's `pm_sleep_disable_secondary_cpus` failing (`suspend.c:437-439`),
    /// which jumps straight to `Enable_cpus` (`:466-467`): `pm_sleep_enable_secondary_cpus` runs, then falls through
    /// `Platform_wake` (`:469-471`). No syscore resume (never reached), and the IRQ disable of `:441` was never
    /// reached either.
    ParkCpus,
    /// The ask inside `syscore_suspend` found a wake before its first callback (`syscore.c:55-57`), with the CPUs
    /// parked (`suspend.c:437`) and the interrupts off (`:441`): no syscore resume, because none is owed (`:447`,
    /// `:458`), but the system is put back to running first (`:461-467`), then `wake` (`:470`), then the devices
    /// (`:471`). With `ParkCpus` it is one of the two refusals that run `resume_cpus` with no syscore
    /// resume before it (and `AfterSyscore` is the one that restores the CPUs after a syscore resume).
    BeforeSyscore,
    /// A wake was pending after the syscore callbacks, before the entry (`suspend.c:448`, `:455-456`). Only a machine
    /// with a platform's way in gets here: where none is registered the syscore points do not run. Its row owes the
    /// syscore resume first (`:458`) and then the same list as `BeforeSyscore`.
    AfterSyscore,
    /// The entry was taken and a wake ended it: the sleep's own way back.
    Slept,
}

/// One step of the way back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// `syscore_resume` (`suspend.c:458`), which owes a resume to each callback whose suspend ran.
    SyscoreResume,
    /// Putting the system back to running, with IRQs on and the secondary CPUs up: Linux's `system_state =
    /// SYSTEM_RUNNING` (`suspend.c:461`), `arch_suspend_enable_irqs()` (`:463`) and
    /// `pm_sleep_enable_secondary_cpus()` (`:467`), which contract v3 §1 makes a hook of its own. On a path that owes
    /// a syscore resume it runs after it (`:458`), never before, and before the platform's own wake (`:470`). Two paths
    /// run it with no syscore resume before it: the ask inside `syscore_suspend`, where the `if (!error)` guard
    /// (`:447`) skipped `:458` but the way out is still all of `:461-467`, and a `park_cpus` refusal (`:437-439`),
    /// which jumps to `Enable_cpus` (`:466-467`) and so runs `:467` alone without the system state and the interrupts
    /// of `:461-464` (contract v3 §2).
    ResumeCpus,
    /// The platform's own wake: `platform_resume_noirq` (`suspend.c:470`, the helper at `:278-286`), which calls
    /// `suspend_ops->wake` (`:283-284`). Contract v3 §1: the second half of what v2 folds into one step, run after
    /// `resume_cpus` and before the devices' noirq resume (`:471`).
    PlatformWake,
    /// What the platform step is on a machine that registers no platform: `platform_resume_noirq` for
    /// `PM_SUSPEND_TO_IDLE` is `s2idle_ops->restore_early` (`suspend.c:280-282`), the step `:470` runs, with the way
    /// out of the loop itself (`:139-151`) behind it.
    S2IdleLeave,
    /// `dpm_resume_noirq` and the walk back from there (`suspend.c:471`, `main.c:753`, `:1007`, `:1106`), which is
    /// also what the caller's `Resume_devices` runs for the refusals above `Platform_wake` (`:520-522`).
    Devices,
}

/// `Close` (`suspend.c:528-529`), which a `platform_suspend_begin` refusal jumps to (`:501-503`): the resume list is
/// not entered at all, so nothing is given back. The way down stopped before any device was touched, so the sequence
/// has nothing to undo either.
const BEGIN: [Step; 0] = [];

/// The devices back and nothing else of this vocabulary. Linux answers these refusals with `Recover_platform`
/// (`suspend.c:533-535`), `Platform_early_resume` (`:473-474`) and `Platform_finish` (`:479-480`). `Platform_early_resume`
/// falls through `Devices_early_resume` (`:476-477`, `dpm_resume_early`) and `Platform_finish` returns to the caller
/// (`:481`, `:522`); `Recover_platform` is answered by the caller's `Resume_devices` and its `dpm_resume_end`
/// (`:520-522`). None of them passes `Platform_wake` (`:469`), so no platform wake and none of the CPU work of
/// `:461-467` runs. What they run besides the devices is Linux's own platform work: `platform_recover` (`:534`, the
/// helper at `:318-322`), `platform_resume_early` (`:474`, the helper at `:288-292`) and `platform_resume_finish`
/// (`:480`, the helper at `:294-298`), which are a `suspend_ops->recover`, an `s2idle_ops->restore` and a
/// `suspend_ops->finish`. This port's `PlatformOps` has no hook for any of the three: they are limits of this port
/// (see the module doc), with no counterpart here, and the row is the devices alone.
const DEVICES_BACK: [Step; 1] = [Step::Devices];

/// `Platform_wake` (`suspend.c:469-471`): the platform's own wake (`:470`, `suspend_ops->wake` at `:283-284`), then
/// the devices (`:471`). `Enable_cpus` (`:466-467`), the CPU and interrupt work, is above this label, so a refusal
/// that jumps here runs no `resume_cpus`; that is the difference between this row and `BEFORE_PLATFORM`.
const WAKE_PLATFORM: [Step; 2] = [Step::PlatformWake, Step::Devices];

/// The row of the refusals that owe the CPU step but no syscore resume. §2's aborted entry with the wake pending inside
/// `syscore_suspend` (`syscore.c:55-57`): the `if (!error)` guard (`suspend.c:447`) skips `syscore_resume` (`:458`),
/// and the way out is the system back to running (`:461-467`), the platform's wake (`:470`) and the devices (`:471`).
/// A `park_cpus` refusal (`:437-439`) goes to `Enable_cpus` (`:466-467`) and falls through the same `Platform_wake`
/// (`:469-471`), so it has the same row. Both only run it after `park_cpus` was asked, so it is owed.
const BEFORE_PLATFORM: [Step; 3] = [Step::ResumeCpus, Step::PlatformWake, Step::Devices];

/// §2's normal wake, steps 4 to 7: `syscore_resume` (`suspend.c:458`), the system back to running (`:461-467`), the
/// platform's own wake (`:470`) and the devices (`:471`).
const AFTER_PLATFORM: [Step; 4] = [
    Step::SyscoreResume,
    Step::ResumeCpus,
    Step::PlatformWake,
    Step::Devices,
];

/// The wake of a sleep that was taken: what the resume entry of `suspend.c:452` runs - the same four steps in the
/// same order - so §2 gives this the row of a normal wake. 1341 had the platform first and the syscore resumes after
/// it, the v2 deviation §1's split removes; this row answers the question its `leave()` left open.
const SLEPT_PLATFORM: [Step; 4] = AFTER_PLATFORM;

/// The s2idle way back, once the entry was made: no syscore resume, because the path ran no syscore ops at all (§3,
/// and `suspend.c:432-434` is the jump that says so), and no `resume_cpus`, because s2idle keeps every CPU online and
/// never parks it. What is left is `platform_resume_noirq` for the s2idle state, `s2idle_ops->restore_early`
/// (`:280-282`), and the devices (`:471`).
const WAY_BACK_S2IDLE: [Step; 2] = [Step::S2IdleLeave, Step::Devices];

/// The steps the way back runs, in order, for an attempt that stopped at `stop` on a machine with a platform's way in
/// (`platform`), or with only the s2idle switch. `Enter` and `Begin` run none. `ParkCpus`, `BeforeSyscore` and
/// `AfterSyscore` on a machine with no platform are rows that cannot happen - its CPUs are never parked and its
/// syscore points never ran (§3) - and are kept for the table's sake, with the s2idle order. Each row is the steps of
/// this vocabulary of the Linux label, not every step the label runs (the limits in the module doc).
pub fn way_back(stop: Stop, platform: bool) -> &'static [Step] {
    match (stop, platform) {
        (Stop::Enter, _) => &[],
        (Stop::Begin, _) => &BEGIN,
        (Stop::DevicesDown, _) => &DEVICES_BACK,
        (Stop::PlatformPrepare, _) => &DEVICES_BACK,
        (Stop::PlatformWake, true) => &WAKE_PLATFORM,
        (Stop::PlatformWake, false) => &WAY_BACK_S2IDLE,
        (Stop::ParkCpus, true) => &BEFORE_PLATFORM,
        (Stop::ParkCpus, false) => &WAY_BACK_S2IDLE,
        (Stop::BeforeSyscore, true) => &BEFORE_PLATFORM,
        (Stop::BeforeSyscore, false) => &WAY_BACK_S2IDLE,
        (Stop::AfterSyscore, true) => &AFTER_PLATFORM,
        (Stop::AfterSyscore, false) => &WAY_BACK_S2IDLE,
        (Stop::Slept, true) => &SLEPT_PLATFORM,
        (Stop::Slept, false) => &WAY_BACK_S2IDLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every path the entry area can end on, with and without a `PlatformOps`.
    const STOPS: [Stop; 9] = [
        Stop::Enter,
        Stop::Begin,
        Stop::DevicesDown,
        Stop::PlatformPrepare,
        Stop::PlatformWake,
        Stop::ParkCpus,
        Stop::BeforeSyscore,
        Stop::AfterSyscore,
        Stop::Slept,
    ];

    /// The refusals above `Platform_wake` (`suspend.c:469`), which is where `resume_cpus` (`:461-467`) stops being
    /// owed: Linux's gotos never fall back up to `Enable_cpus` (`:466-467`).
    const ABOVE_ENABLE_CPUS: [Stop; 4] = [
        Stop::Enter,
        Stop::Begin,
        Stop::DevicesDown,
        Stop::PlatformPrepare,
    ];

    /// Where a step sits in the way back of one path, `None` where it does not run.
    fn where_is(stop: Stop, platform: bool, step: Step) -> Option<usize> {
        way_back(stop, platform).iter().position(|s| *s == step)
    }

    #[test]
    fn the_entry_that_is_taken_gives_back_nothing() {
        // `suspend.c:448` asks and gets no, `:452` takes `suspend_ops->enter`; the s2idle path asks at `:143` and
        // enters at `:150`. Nothing of the way back runs from here.
        assert!(way_back(Stop::Enter, true).is_empty());
        assert!(way_back(Stop::Enter, false).is_empty());
    }

    #[test]
    fn the_begin_refusal_never_enters_the_resume_list() {
        // `platform_suspend_begin` failing (`suspend.c:501-503`) jumps to `Close` (`:528-529`), past
        // `Resume_devices` (`:520-522`): no syscore resume, no CPU work, no platform wake and no device resume,
        // because `suspend_enter` was never called and no device was touched.
        assert_eq!(way_back(Stop::Begin, true), &[]);
        assert_eq!(way_back(Stop::Begin, false), &[]);
    }

    #[test]
    fn a_device_that_refuses_gives_the_devices_back_and_no_platform_step() {
        // `dpm_suspend_start` failing (`suspend.c:507-511`) goes to `Recover_platform` (`:533-535`) and from there to
        // the caller's `Resume_devices` and `dpm_resume_end` (`:520-522`); the noirq walk failing (`:420-424`) goes to
        // `Platform_early_resume` (`:473-474`) and reaches the same `dpm_resume_end` by falling through. Neither
        // passes `Platform_wake` (`:469`), so neither the platform's wake (`:470`) nor `Enable_cpus` (`:466-467`)
        // runs: the devices come back and nothing else. `platform_resume_early` (`:474`) is `s2idle_ops->restore` for
        // the s2idle state only (`:288-292`), which this port registers no driver for, and the s2idle switch was
        // never set on this path.
        assert_eq!(way_back(Stop::DevicesDown, true), &[Step::Devices]);
        assert_eq!(way_back(Stop::DevicesDown, false), &[Step::Devices]);
    }

    #[test]
    fn the_platform_prepare_refusal_lands_on_platform_finish() {
        // `platform_suspend_prepare` failing (`suspend.c:407-409`, the helper at `:257-260`) and `dpm_suspend_late`
        // failing (`:411-415`) both go to `Platform_finish` (`:479-480`), which runs `platform_resume_finish` - our
        // `PlatformOps` has no `finish` step - and then the caller's `dpm_resume_end` (`:522`). No `resume_cpus`
        // (`:461-467`), no `wake` (`:470`), no syscore resume (`:458`), which is inside the `if (!error)` of `:447`
        // and was never reached.
        assert_eq!(way_back(Stop::PlatformPrepare, true), &[Step::Devices]);
        assert_eq!(way_back(Stop::PlatformPrepare, false), &[Step::Devices]);
    }

    #[test]
    fn the_last_veto_lands_on_platform_wake_without_the_cpu_step() {
        // `platform_suspend_prepare_noirq` failing (`suspend.c:425-427`) and `suspend_test(TEST_PLATFORM)`
        // (`:429-430`) go to `Platform_wake` (`:469-471`): the platform's own wake (`:470`, `suspend_ops->wake` at
        // `:283-284`) and the devices (`:471`, `main.c:753`) run, but `Enable_cpus` (`:466-467`) is above the label,
        // so `:461-467` does not. This is also where the s2idle path's two asks end (`:143` and `:95`, the jump at
        // `:434` from `s2idle_loop`), and for that state `platform_resume_noirq` is `s2idle_ops->restore_early`
        // (`:280-282`) in the platform's place.
        assert_eq!(
            way_back(Stop::PlatformWake, true),
            &[Step::PlatformWake, Step::Devices]
        );
        assert_eq!(
            way_back(Stop::PlatformWake, false),
            &[Step::S2IdleLeave, Step::Devices]
        );
    }

    #[test]
    fn a_park_refusal_goes_to_enable_cpus_and_falls_through_platform_wake() {
        // `pm_sleep_disable_secondary_cpus` failing (`suspend.c:437-439`) jumps to `Enable_cpus` (`:466-467`), which
        // runs `pm_sleep_enable_secondary_cpus` and falls through `Platform_wake` (`:469-471`): the CPU step, the
        // platform's wake and the devices. `syscore_suspend` (`:446`) was never reached, so no syscore resume (`:458`).
        assert_eq!(
            way_back(Stop::ParkCpus, true),
            &[Step::ResumeCpus, Step::PlatformWake, Step::Devices]
        );
        assert!(!way_back(Stop::ParkCpus, true).contains(&Step::SyscoreResume));
        assert_eq!(
            way_back(Stop::ParkCpus, true),
            way_back(Stop::BeforeSyscore, true)
        );
    }

    #[test]
    fn only_the_stops_past_the_cpu_parking_put_the_cpus_back() {
        // The ask inside `syscore_suspend` (`syscore.c:55-57`) reaches `:461` - Linux has parked the CPUs at
        // `suspend.c:437` and disabled the interrupts at `:441` before it, and the `if (!error)` guard (`:447`) skips
        // only `syscore_resume` (`:458`) - and so does a park refusal (`:437-439`, `Enable_cpus` `:466-467`). So
        // `resume_cpus` is the first step owed there (§2), and it is owed nowhere above `Platform_wake` (`:469`).
        assert_eq!(
            way_back(Stop::BeforeSyscore, true),
            &[Step::ResumeCpus, Step::PlatformWake, Step::Devices]
        );
        for stop in ABOVE_ENABLE_CPUS {
            assert!(
                !way_back(stop, true).contains(&Step::ResumeCpus),
                "{stop:?} runs `resume_cpus`, which no Linux label above `Enable_cpus` (`suspend.c:466-467`) reaches"
            );
        }
        // `Platform_wake` (`:469-471`) runs the platform's wake and not the CPUs.
        assert!(!way_back(Stop::PlatformWake, true).contains(&Step::ResumeCpus));
        // The four stops that do owe it, each with `resume_cpus` before `wake` and before the devices.
        for stop in [
            Stop::ParkCpus,
            Stop::BeforeSyscore,
            Stop::AfterSyscore,
            Stop::Slept,
        ] {
            assert!(
                way_back(stop, true).contains(&Step::ResumeCpus),
                "{stop:?} owes the CPU step of `suspend.c:461-467`"
            );
        }
    }

    #[test]
    fn a_wake_after_the_syscore_points_resumes_them_before_the_cpus_and_the_platform() {
        // `suspend.c:448` finds a wake, `:456` answers `-EBUSY` without `:452`, and the way back is §2's normal wake
        // without the entry: `syscore_resume` (`:458`), then `resume_cpus` (`:461-467`), then
        // `platform_resume_noirq` (`:470`, `:278-286`), then `dpm_resume_noirq` (`:471`, `main.c:753`).
        assert_eq!(
            way_back(Stop::AfterSyscore, true),
            &[
                Step::SyscoreResume,
                Step::ResumeCpus,
                Step::PlatformWake,
                Step::Devices
            ]
        );
        // The s2idle path never gets here, and where it stops it runs no syscore resume either (§3).
        assert_eq!(
            way_back(Stop::AfterSyscore, false),
            &[Step::S2IdleLeave, Step::Devices]
        );
    }

    #[test]
    fn the_wake_of_a_sleep_is_the_normal_wake_of_contract_v3() {
        // §2, the normal wake: the entry of `suspend.c:452` is over and its resume entry runs `syscore_resume`
        // (`:458`) before the system is put back to running (`:461-467`) and the platform's own wake (`:470`), with
        // the devices after them (`:471`). This is what v3 settles for the path 1341 ordered platform-first, and it is
        // the same list §2 gives the entry given up after the callbacks: the same way back, without the entry.
        assert_eq!(
            way_back(Stop::Slept, true),
            &[
                Step::SyscoreResume,
                Step::ResumeCpus,
                Step::PlatformWake,
                Step::Devices
            ]
        );
        assert_eq!(
            way_back(Stop::Slept, true),
            way_back(Stop::AfterSyscore, true)
        );
    }

    #[test]
    fn resume_cpus_sits_between_the_syscore_resume_and_the_platform_wake() {
        // Contract v3 §1: `resume_cpus` "runs after syscore resume (`suspend.c:458`), never before" and `wake` runs
        // after `resume_cpus` and before the devices' noirq resume (`:471`). Every platform path that owes both; the
        // ask inside `syscore_suspend` is the one that owes `resume_cpus` with no syscore resume before it (§2), and
        // `Platform_wake` (`:469-471`) is the one that owes `wake` with no `resume_cpus` at all.
        for stop in STOPS {
            let (syscore, cpus, wake, devices) = (
                where_is(stop, true, Step::SyscoreResume),
                where_is(stop, true, Step::ResumeCpus),
                where_is(stop, true, Step::PlatformWake),
                where_is(stop, true, Step::Devices),
            );
            if let Some(cpus) = cpus {
                let (wake, devices) = (wake.unwrap(), devices.unwrap());
                assert!(cpus < wake && wake < devices, "{stop:?}");
                if let Some(syscore) = syscore {
                    assert!(syscore < cpus, "{stop:?}");
                }
            } else {
                assert!(syscore.is_none(), "{stop:?}");
                if let (Some(wake), Some(devices)) = (wake, devices) {
                    assert!(wake < devices, "{stop:?}");
                }
            }
        }
    }

    #[test]
    fn the_syscore_resumes_run_only_where_the_syscore_suspends_ran() {
        for stop in STOPS {
            for platform in [true, false] {
                // A syscore resume is owed only where the suspends ran and completed: on the platform path, past
                // `syscore_suspend` (`suspend.c:446`, `:458`); never on the s2idle path, which jumps past it
                // (`:432-434`, contract v3 §3); and never at the ask inside `syscore_suspend`, where the `if (!error)`
                // guard (`:447`) skips `:458` because no suspend completed.
                let ran_syscore = platform && matches!(stop, Stop::AfterSyscore | Stop::Slept);
                assert_eq!(
                    way_back(stop, platform).contains(&Step::SyscoreResume),
                    ran_syscore,
                    "{stop:?} with platform {platform}"
                );
            }
        }
    }

    #[test]
    fn the_s2idle_path_runs_no_syscore_and_no_cpu_step() {
        // Contract v3 §3: no syscore ops on the s2idle path, and no `resume_cpus` either - Linux's `s2idle_enter`
        // (`suspend.c:90-124`) keeps every CPU online and pushes them into the idle loop (`:104`), so there is no
        // parked CPU and no syscore work to bring back. Where the entry was made its way back is the step out of it
        // (`:280-282`) and then the devices (`:471`); where a device refused earlier, `:288-292` and `:294-298` have
        // no s2idle driver to call and nothing of ours was set.
        for stop in STOPS {
            for step in [Step::SyscoreResume, Step::ResumeCpus, Step::PlatformWake] {
                assert!(
                    !way_back(stop, false).contains(&step),
                    "{stop:?} runs {step:?} on the s2idle path"
                );
            }
        }
        assert_eq!(
            way_back(Stop::Slept, false),
            &[Step::S2IdleLeave, Step::Devices]
        );
    }

    #[test]
    fn every_way_back_but_the_entry_and_the_begin_ends_with_the_devices() {
        for stop in STOPS {
            for platform in [true, false] {
                let back = way_back(stop, platform);
                if stop == Stop::Enter || stop == Stop::Begin {
                    // `Close` (`suspend.c:528-529`) is reached without a device touched, so there is no walk to undo.
                    assert!(back.is_empty(), "{stop:?} with platform {platform}");
                } else {
                    assert_eq!(back.last(), Some(&Step::Devices), "{stop:?}");
                }
            }
        }
    }
}
