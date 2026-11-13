//! docs/spec/kmsg-wake.md: the kmsg wake model's configurations and its witnesses.

use lucina_models::kmsg_wake::{Ctx, KmsgWake, Variant};
use lucina_models::{check, explain};

const SAFETY: [&str; 2] = [
    "no lost wake-up",
    "the reader reads only what is in the ring",
];
const EXAMPLES: [&str; 3] = [
    "the reader sleeps and is woken for a record",
    "a raise is coalesced",
    "the reader reads every record",
];

fn holds(m: KmsgWake) {
    let what = format!("{m:?}");
    let out = check(m.clone());
    println!(
        "{what}: {} states, {} unique, found {:?}",
        out.states, out.unique, out.discovered
    );
    for p in SAFETY {
        assert!(
            !out.discovered.contains(&p),
            "{what}: `{p}` fails:\n{}",
            explain(m.clone(), p).unwrap_or_default()
        );
    }
    for p in EXAMPLES {
        assert!(
            out.discovered.contains(&p),
            "{what}: never reached `{p}`: the model is vacuous there"
        );
    }
}

fn fails(m: KmsgWake, property: &'static str) {
    let what = format!("{m:?}");
    let out = check(m.clone());
    println!("{what}: {} states, found {:?}", out.states, out.discovered);
    assert!(
        out.discovered.contains(&property),
        "witness {what}: `{property}` held, the model cannot see this bug"
    );
    println!(
        "counterexample for `{property}`:\n{}",
        explain(m, property).unwrap_or_default()
    );
}

/// Every placement of two writers: two CPUs or one, tasks and interrupts.
fn configurations() -> Vec<Vec<(usize, Ctx)>> {
    use Ctx::*;
    vec![
        vec![(0, Task), (1, Task)],
        vec![(0, Task), (1, Irq)],
        vec![(0, Irq), (1, Irq)],
        vec![(0, Task), (0, Task)],
        vec![(0, Task), (0, Irq)],
        vec![(0, Irq), (0, Irq)],
    ]
}

#[test]
fn shipped_holds_two_writers_everywhere() {
    for writers in configurations() {
        holds(KmsgWake::shipped(writers));
    }
}

#[test]
fn shipped_holds_three_writers_on_two_cpus() {
    use Ctx::*;
    holds(KmsgWake::shipped(vec![(0, Task), (1, Irq), (0, Irq)]));
    holds(KmsgWake::shipped(vec![(0, Task), (1, Task), (1, Task)]));
}

/// The reader that looks before it registers and then sleeps: a record logged between the look and the registration,
/// woken for before the poller was there, is never woken for again.
#[test]
fn witness_check_before_register_loses_a_wake() {
    fails(
        KmsgWake::shipped(vec![(0, Ctx::Task)]).with(Variant::CheckBeforeRegister),
        "no lost wake-up",
    );
}

/// The softirq that wakes and then clears the flag: a writer between the two sees the flag on and raises nothing.
#[test]
fn witness_notify_before_clear_loses_a_wake() {
    fails(
        KmsgWake::shipped(vec![(0, Ctx::Task), (1, Ctx::Task)]).with(Variant::NotifyBeforeClear),
        "no lost wake-up",
    );
}

/// The writer that sets the flag before its record is in the ring: the wake can run, and the reader look, in between.
#[test]
fn witness_flag_before_publish_loses_a_wake() {
    fails(
        KmsgWake::shipped(vec![(0, Ctx::Task)]).with(Variant::FlagBeforePublish),
        "no lost wake-up",
    );
}

/// A task writer without the SGI: its CPU runs the softirq at its next interrupt, which an idle CPU with its tick stopped
/// may not get for a long time.
#[test]
fn witness_task_writer_without_sgi_loses_a_wake() {
    fails(
        KmsgWake::shipped(vec![(0, Ctx::Task)]).with(Variant::NoSgi),
        "no lost wake-up",
    );
}

/// An interrupt writer needs no SGI: the interrupt's exit runs the softirq.
#[test]
fn irq_writers_hold_without_sgi() {
    use Ctx::*;
    holds(KmsgWake::shipped(vec![(0, Irq), (1, Irq)]).with(Variant::NoSgi));
}

/// The delivery path the next witness breaks: a raise on a CPU whose softirq run is in progress.
const RAISE_IN_RUN: &str = "a raise lands during its CPU's softirq run";

/// The softirq loop that does not read the pending bits again after its callbacks: a writer on CPU 0 raises, the run
/// takes the flag, an interrupt writer nested in that run turns the flag on again and sets the bit, and the run ends
/// with the bit set and nothing to process it. The flag stays on, so a third writer, on another CPU, raises nothing.
/// The shipped loop, in the same configuration, reaches that raise and holds.
#[test]
fn witness_softirq_loop_without_recheck_loses_a_wake() {
    use Ctx::*;
    let writers = vec![(0, Task), (0, Irq), (1, Task)];
    let shipped = KmsgWake::shipped(writers.clone());
    assert!(
        check(shipped.clone()).discovered.contains(&RAISE_IN_RUN),
        "{shipped:?}: never reached `{RAISE_IN_RUN}`: the witness would break a path the model does not have"
    );
    holds(shipped);
    fails(
        KmsgWake::shipped(writers).with(Variant::NoRecheckAfterRun),
        "no lost wake-up",
    );
}

/// Where the shipped design depends on the loop's second look: every configuration with two writers on one CPU reaches
/// a raise during that CPU's softirq run. With one writer per CPU no run can be in progress when its CPU's only raise
/// comes, so the path is not there.
#[test]
fn shipped_reaches_a_raise_during_a_softirq_run() {
    use Ctx::*;
    for writers in configurations().into_iter().chain([
        vec![(0, Task), (1, Irq), (0, Irq)],
        vec![(0, Task), (1, Task), (1, Task)],
    ]) {
        let shared_cpu = writers.iter().filter(|w| w.0 == writers[0].0).count() > 1
            || writers.iter().filter(|w| w.0 == writers[1].0).count() > 1;
        let m = KmsgWake::shipped(writers);
        let reached = check(m.clone()).discovered.contains(&RAISE_IN_RUN);
        assert_eq!(reached, shared_cpu, "{m:?}: `{RAISE_IN_RUN}`");
    }
}
