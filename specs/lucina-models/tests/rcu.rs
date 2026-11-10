//! docs/spec/rcu.md: the RCU model's configurations, its witnesses, and its bookkeeping against the shipped crate.

use lucina_models::rcu::{IdleQs, Mon, Rcu, Tick};
use lucina_models::{check, explain};

const SAFETY: [&str; 2] = ["no use after free", "the published object is never freed"];
const BOUND: &str = "freed within the bound";
const EXAMPLES: [&str; 3] = [
    "an object is freed",
    "an object is freed while a CPU idles",
    "an idle CPU reads in an interrupt",
];

fn holds(m: Rcu) {
    let what = format!("{m:?}");
    let out = check(m.clone());
    println!(
        "{what}: {} states, {} unique, found {:?}",
        out.states, out.unique, out.discovered
    );
    for p in SAFETY.iter().chain([BOUND].iter()) {
        assert!(
            !out.discovered.contains(p),
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

fn fails(m: Rcu, property: &'static str) {
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

#[test]
fn shipped_with_periodic_tick_holds_2_cpus() {
    holds(Rcu::shipped(2));
}

#[test]
fn shipped_with_periodic_tick_holds_3_cpus() {
    holds(Rcu::shipped(3));
}

/// Finding RCU-1: with the idle tick stopped, a CPU that went idle before a grace period started holds it up until something
/// wakes it. Nothing bounds that, so freed memory (the frames of exited processes) waits as long as the CPU sleeps.
#[test]
fn witness_tickless_idle_holds_up_grace_periods() {
    fails(
        Rcu {
            tick: Tick::Tickless,
            ..Rcu::shipped(2)
        },
        BOUND,
    );
}

/// The proposed fix, Linux's extended quiescent state, meets the bound with the tick stopped, and stays safe.
#[test]
fn extended_qs_holds_tickless_2_cpus() {
    holds(Rcu {
        tick: Tick::Tickless,
        idle: IdleQs::Extended,
        ..Rcu::shipped(2)
    });
}

#[test]
fn extended_qs_holds_tickless_3_cpus() {
    holds(Rcu {
        tick: Tick::Tickless,
        idle: IdleQs::Extended,
        ..Rcu::shipped(3)
    });
}

/// The extended state is only safe if an interrupt leaves it before its handler reads.
#[test]
fn witness_extended_qs_irq_must_leave_it() {
    fails(
        Rcu {
            tick: Tick::Tickless,
            idle: IdleQs::ExtendedIrqStays,
            ..Rcu::shipped(2)
        },
        SAFETY[0],
    );
}

/// Assumption K matters: a kernel loop that never reaches a quiescent point holds up every grace period.
#[test]
fn witness_kernel_loop_without_qs() {
    fails(
        Rcu {
            kernel_qs_per_tick: false,
            ..Rcu::shipped(2)
        },
        BOUND,
    );
}

/// The model's bookkeeping is the shipped `lucina_rcu::Monitor`: every sequence of up to 7 calls (retire, a quiescent
/// state on one of 3 CPUs, take_ready) hands out the same items and leaves the same `is_monitoring`.
#[test]
fn mon_matches_lucina_rcu_monitor() {
    const OPS: u32 = 5; // retire, quiescent(0..3), take_ready
    const LEN: u32 = 7;
    const ALL: u64 = 0b111;
    let mut sequences = 0u64;
    for code in 0..OPS.pow(LEN) {
        let mut real = lucina_rcu::Monitor::<u8>::new();
        let mut model = Mon::new();
        let mut next_item = 0u8;
        let mut c = code;
        for step in 0..LEN {
            let op = c % OPS;
            c /= OPS;
            match op {
                0 => {
                    real.retire(next_item);
                    model.retire(next_item, 0);
                    next_item += 1;
                }
                1..=3 => {
                    real.quiescent(op - 1, ALL);
                    model.quiescent(op - 1, ALL, 0, false);
                }
                _ => {
                    let mut a = real.take_ready();
                    let mut b = model.take_ready();
                    a.sort_unstable();
                    b.sort_unstable();
                    assert_eq!(a, b, "sequence {code}, step {step}: take_ready differs");
                }
            }
            assert_eq!(
                real.is_monitoring(),
                model.monitoring,
                "sequence {code}, step {step}: is_monitoring differs"
            );
        }
        // What is still waiting must come out the same way too.
        for cpu in 0..3 {
            real.quiescent(cpu, ALL);
            model.quiescent(cpu, ALL, 0, false);
        }
        let mut a = real.take_ready();
        let mut b = model.take_ready();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "sequence {code}: final take_ready differs");
        sequences += 1;
    }
    println!("{sequences} sequences agree");
}
