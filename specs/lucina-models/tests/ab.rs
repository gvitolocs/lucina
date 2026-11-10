//! docs/spec/juno-ab-boot.md: Juno's A/B generations under power cuts, the proposed design and the shipped one's findings.

use lucina_models::ab::{Ab, Behaviour};
use lucina_models::{check, explain};

const SAFETY: [&str; 5] = [
    "BOOT.CFG names a whole generation",
    "PREV.CFG names a whole generation",
    "the loader always starts something",
    "the loader never starts a generation whose root is gone",
    "the running root is never written",
];
const EARLY: &str = "a generation that never lasts starts at most TRIES times";
const LATE: &str = "a generation that dies after its good mark is sent back in bounded boots";

fn run(m: &Ab) -> Vec<&'static str> {
    let out = check(m.clone());
    println!(
        "{m:?}: {} states, {} unique, found {:?}",
        out.states, out.unique, out.discovered
    );
    out.discovered
}

fn holds(m: Ab, examples: &[&'static str]) {
    let found = run(&m);
    for p in SAFETY.iter().chain([EARLY, LATE].iter()) {
        assert!(
            !found.contains(p),
            "`{p}` fails:\n{}",
            explain(m.clone(), p).unwrap_or_default()
        );
    }
    for p in examples {
        assert!(
            found.contains(p),
            "never reached `{p}`: the model is vacuous there"
        );
    }
}

fn fails(m: Ab, property: &'static str) {
    let found = run(&m);
    assert!(
        found.contains(&property),
        "witness {m:?}: `{property}` held, the model cannot see this bug"
    );
    println!(
        "counterexample for `{property}`:\n{}",
        explain(m, property).unwrap_or_default()
    );
}

#[test]
fn proposed_holds_two_installs() {
    holds(
        Ab::proposed(),
        &[
            "a new generation is marked good",
            "the loader starts PREV.CFG",
            "a second install replaces PREV.CFG's slot",
        ],
    );
}

#[test]
fn proposed_holds_late_failures() {
    holds(
        Ab {
            max_gen: 2,
            max_boots: 10,
            behaviours: vec![Behaviour::Good, Behaviour::CrashLate],
            ..Ab::proposed()
        },
        &[
            "a new generation is marked good",
            "the loader starts PREV.CFG",
        ],
    );
}

/// The shipped code since 2026-11-08 (AB-1, AB-2 and AB-3 fixed): the same guarantees as the proposed design.
#[test]
fn shipped_holds_two_installs() {
    holds(
        Ab::shipped(),
        &[
            "a new generation is marked good",
            "the loader starts PREV.CFG",
        ],
    );
}

#[test]
fn shipped_holds_late_failures() {
    holds(
        Ab {
            max_gen: 2,
            max_boots: 10,
            behaviours: vec![Behaviour::Good, Behaviour::CrashLate],
            ..Ab::shipped()
        },
        &[
            "a new generation is marked good",
            "the loader starts PREV.CFG",
        ],
    );
}

/// AB-1: the second install writes over the slot PREV.CFG boots before it removes PREV.CFG.
#[test]
fn finding_ab1_prev_names_an_overwritten_root() {
    fails(
        Ab {
            drop_prev_first: false,
            ..Ab::proposed()
        },
        SAFETY[1],
    );
}

/// AB-2: the target slot follows BOOT.CFG, not the running root; after a settle rollback the running generation's own
/// root is the target.
#[test]
fn finding_ab2_install_writes_the_running_root() {
    fails(
        Ab {
            install_checks_running: false,
            ..Ab::proposed()
        },
        SAFETY[4],
    );
}

/// AB-3: after the good mark only lucina-trial counts bad boots; a generation that dies before userspace is never sent back.
#[test]
fn finding_ab3_early_death_after_good_mark_never_counted() {
    fails(
        Ab {
            max_gen: 2,
            max_boots: 10,
            behaviours: vec![Behaviour::CrashLate],
            settle_in_loader: false,
            ..Ab::proposed()
        },
        LATE,
    );
}

/// The loader boots without counting when it cannot write the record: a generation that never lasts then starts forever.
#[test]
fn witness_record_writes_fail() {
    fails(
        Ab {
            max_gen: 2,
            behaviours: vec![Behaviour::CrashEarly],
            record_writes_fail: true,
            ..Ab::proposed()
        },
        EARLY,
    );
}

/// Assumption D1 (one block lands whole): without it a cut in the commit leaves no BOOT.CFG the loader can read.
#[test]
fn witness_torn_commit() {
    fails(
        Ab {
            max_gen: 2,
            torn_commit: true,
            ..Ab::proposed()
        },
        SAFETY[2],
    );
}
