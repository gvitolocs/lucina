// When a spin lock's waiter speaks up, and which CPUs can hold the lock it waits for (patch 1214). Verus checks this very
// file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011); verification/juno-spinwatch/mutants.sh plants bugs
// that Verus must refuse. The kernel (ostd/src/sync/spinwatch.rs) calls these functions and holds no copy of the rules.
//
// A CPU that holds a spin lock with interrupts off and never lets go stops every CPU that spins behind it, interrupts off too:
// no timer, no soft-lockup thread and no watchdog bark runs any more, and the firmware resets the board with nothing recorded.
// The only code that still runs is the waiters' own loop, so the loop itself reads the counter: after a while it marks the
// wait, and later it reports it or ends the system with a record that names the lock.

/// After this long a wait is marked as stalled, for the crash record.
pub const MARK_SECONDS: u64 = 1;
/// After this long a wait is reported, or ends the system: long before the laptop's firmware resets it, and longer than any
/// lock is held on purpose.
pub const ACT_SECONDS: u64 = 5;

/// The times of [`decide`] for a counter of `frequency` Hz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub mark_after: u64,
    pub act_after: u64,
}

impl Thresholds {
    /// A counter of 0 Hz (not set up) never gets there.
    pub fn for_frequency(frequency: u64) -> Self {
        if frequency == 0 {
            return Self {
                mark_after: u64::MAX,
                act_after: u64::MAX,
            };
        }
        Self {
            mark_after: frequency.saturating_mul(MARK_SECONDS),
            act_after: frequency.saturating_mul(ACT_SECONDS),
        }
    }
}

/// What `lucina.spinwatch=` asks for.
#[derive(Clone, Copy)]
// Verus reads only the derive above.
#[allow(dead_code)] // an attribute between the two derives keeps rustfmt's merge_derives from joining them
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Mode {
    /// Nothing at all.
    Off,
    /// A line in the log, once per wait.
    Report,
    /// A crash with the waits in its record (the default).
    Crash,
}

/// What a wait has done so far.
#[derive(Clone, Copy)]
// Verus reads only the derive above.
#[allow(dead_code)] // an attribute between the two derives keeps rustfmt's merge_derives from joining them
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Stage {
    Quiet,
    /// Its slot says it is stalled.
    Marked,
    /// It said so in the log, or went for the crash.
    Reported,
}

/// What a waiter does at a check.
#[derive(Clone, Copy)]
// Verus reads only the derive above.
#[allow(dead_code)] // an attribute between the two derives keeps rustfmt's merge_derives from joining them
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Action {
    Nothing,
    /// Mark its slot as stalled.
    Mark,
    /// Say so in the log.
    Report,
    /// End the system.
    Crash,
}

//@ pub open spec fn waited(now: u64, start: u64) -> int {
//@     if now >= start { now - start } else { 0 }
//@ }

/// How long a wait that started at `start` has lasted at `now`, in counter ticks. A counter that reads earlier than the start
/// (another CPU's, a little behind) counts as no wait.
pub fn waited_ticks(now: u64, start: u64) -> u64 //@ret
//@ ensures ret as int == waited(now, start),
{
    now.saturating_sub(start)
}

/// What a waiter does at `now` after waiting since `start`: after `mark_after` ticks it marks its slot once, after `act_after`
/// it reports once (`Report`) or crashes (`Crash`). `Off` never does anything.
pub fn decide(
    now: u64,
    start: u64,
    mark_after: u64,
    act_after: u64,
    mode: Mode,
    stage: Stage,
) -> Action //@ret
//@ ensures
//@     mode is Off ==> ret is Nothing,
//@     ret is Crash ==> mode is Crash && waited(now, start) >= act_after,
//@     ret is Report ==> mode is Report && waited(now, start) >= act_after && !(stage is Reported),
//@     ret is Mark ==> !(mode is Off) && stage is Quiet && mark_after <= waited(now, start) < act_after,
//@     (mode is Crash && waited(now, start) >= act_after) ==> ret is Crash,
//@     (mode is Report && waited(now, start) >= act_after && !(stage is Reported)) ==> ret is Report,
//@     (!(mode is Off) && stage is Quiet && mark_after <= waited(now, start) < act_after) ==> ret is Mark,
{
    if let Mode::Off = mode {
        return Action::Nothing;
    }
    let w = waited_ticks(now, start);
    if w >= act_after {
        return match mode {
            Mode::Crash => Action::Crash,
            _ => match stage {
                Stage::Reported => Action::Nothing,
                _ => Action::Report,
            },
        };
    }
    if w >= mark_after {
        return match stage {
            Stage::Quiet => Action::Mark,
            _ => Action::Nothing,
        };
    }
    Action::Nothing
}

/// The stage after `action`: a mark is not done again, a report is the last word.
pub fn next_stage(stage: Stage, action: Action) -> Stage //@ret
//@ ensures
//@     action is Nothing ==> ret == stage,
//@     action is Mark ==> ret is Marked,
//@     (action is Report || action is Crash) ==> ret is Reported,
{
    match action {
        Action::Nothing => stage,
        Action::Mark => Stage::Marked,
        Action::Report | Action::Crash => Stage::Reported,
    }
}

/// Whether a CPU can hold a lock, derived afterwards from what the CPUs wait for: nothing is recorded when a lock is taken.
#[derive(Clone, Copy)]
// Verus reads only the derive above.
#[allow(dead_code)] // an attribute between the two derives keeps rustfmt's merge_derives from joining them
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Holder {
    /// No such CPU, or it waits for this very lock (a CPU that takes a lock twice waits for itself, which this cannot tell).
    No,
    /// It waits for nothing: the likely holder.
    Free,
    /// It waits for another lock, so it may hold this one while it waits for that one.
    Nested,
}

/// Whether CPU `cpu` can hold `lock`, given `waits`, the lock address each CPU waits for (0 for none).
pub fn holder_tier(waits: &[usize], lock: usize, cpu: usize) -> Holder //@ret
//@ ensures
//@     ret is Free <==> (cpu < waits@.len() && lock != 0 && waits@[cpu as int] == 0),
//@     ret is Nested <==> (cpu < waits@.len() && lock != 0 && waits@[cpu as int] != 0 && waits@[cpu as int] != lock),
//@     ret is No <==> (cpu >= waits@.len() || lock == 0 || waits@[cpu as int] == lock),
{
    if cpu >= waits.len() || lock == 0 || waits[cpu] == lock {
        return Holder::No;
    }
    if waits[cpu] == 0 {
        Holder::Free
    } else {
        Holder::Nested
    }
}

//@ pub open spec fn waiters_of(waits: Seq<usize>, lock: usize, n: int) -> int
//@     decreases n,
//@ {
//@     if n <= 0 { 0 } else { waiters_of(waits, lock, n - 1) + if lock != 0 && waits[n - 1] == lock { 1int } else { 0int } }
//@ }

/// How many CPUs wait for `lock` (0 for none counts nothing).
pub fn waiters(waits: &[usize], lock: usize) -> usize //@ret
//@ ensures ret as int == waiters_of(waits@, lock, waits@.len() as int),
{
    let mut count: usize = 0;
    let mut k: usize = 0;
    while k < waits.len()
    //@ invariant
    //@     k <= waits@.len(),
    //@     count as int == waiters_of(waits@, lock, k as int),
    //@     count <= k,
    //@ decreases waits@.len() - k,
    {
        if lock != 0 && waits[k] == lock {
            count += 1;
        }
        k += 1;
    }
    count
}
