//! Pure decisions, separate from the processes around them so that the tests can cover every case: what a run's facts mean, and
//! whether a commit may be merged.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Verified,
    ProofFailed,
    /// Exit 0 but Verus checked nothing: compiling is not verifying.
    Vacuous,
    MutantSurvived,
    Oom,
    Timeout,
    Infra,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Verified => "verified",
            Outcome::ProofFailed => "proof_failed",
            Outcome::Vacuous => "vacuous",
            Outcome::MutantSurvived => "mutant_survived",
            Outcome::Oom => "oom",
            Outcome::Timeout => "timeout",
            Outcome::Infra => "infra_error",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Verified => 0,
            Outcome::ProofFailed | Outcome::Vacuous | Outcome::MutantSurvived => 1,
            Outcome::Infra => 2,
            Outcome::Oom => 3,
            Outcome::Timeout => 4,
        }
    }
}

pub struct Facts<'a> {
    pub kind: &'a str,
    pub exit_code: i32,
    pub oom_kills: u64,
    pub timed_out: bool,
    pub verified: Option<u64>,
    pub errors: Option<u64>,
}

/// Order matters. A timeout or an OOM kill decides first: a mutant script whose Verus was killed exits non-zero exactly like one
/// whose mutant was refused, and that must never count as a refusal.
pub fn classify(f: &Facts) -> Outcome {
    if f.timed_out {
        return Outcome::Timeout;
    }
    if f.oom_kills > 0 {
        return Outcome::Oom;
    }
    match f.kind {
        "verus" | "shipped" | "fixture" => {
            if f.exit_code != 0 || f.errors.is_some_and(|e| e > 0) {
                Outcome::ProofFailed
            } else if f.verified.unwrap_or(0) == 0 {
                Outcome::Vacuous
            } else {
                Outcome::Verified
            }
        }
        "mutants" | "mutations" => {
            if f.exit_code == 0 {
                Outcome::Verified
            } else {
                Outcome::MutantSurvived
            }
        }
        _ => {
            if f.exit_code == 0 {
                Outcome::Verified
            } else {
                Outcome::ProofFailed
            }
        }
    }
}

/// What the log must show for a unit that exited 0 to have proved something. A run that passed on its exit code alone is not evidence.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OutputCheck {
    /// Why the log does not show a real pass.
    pub problem: Option<String>,
    /// Mutants refused without a verification run (a compile or other error): weak evidence, reported per unit.
    pub weak: Option<u64>,
}

/// `models`: Stateright's tests ran (`test result: ok. N passed`, N > 0 in total, no FAILED). `tla`: at least one `ok  <model> (` line and
/// no FAIL line. `deep`: states were counted. `full`: Verus verified something. `mutants`: the unmutated files verified first
/// (`baseline: verifies` exactly once, before the first `refused`/`NOT REFUSED` line) and the closing `mutants: N refused by
/// verification, W refused without a verification run` line is there. Other kinds have their own checks in `classify`.
pub fn check_output(kind: &str, log: &str) -> OutputCheck {
    let problem = |s: &str| OutputCheck {
        problem: Some(s.to_string()),
        weak: None,
    };
    match kind {
        "models" => {
            let mut passed = 0u64;
            for l in log.lines() {
                if l.contains("test result: FAILED") {
                    return problem("a test result says FAILED");
                }
                if let Some(rest) = l.split("test result: ok. ").nth(1) {
                    passed += rest
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<u64>().ok())
                        .unwrap_or(0);
                }
            }
            if passed == 0 {
                return problem("exit 0, but no test reported passing: nothing ran");
            }
            OutputCheck::default()
        }
        "tla" => {
            if log
                .lines()
                .any(|l| l.starts_with("FAIL") || l.starts_with("BAD"))
            {
                return problem("a model line says FAIL");
            }
            if !log
                .lines()
                .any(|l| l.starts_with("ok ") && l.contains(" ("))
            {
                return problem("exit 0, but no model reported ok: nothing was checked");
            }
            OutputCheck::default()
        }
        "deep" => {
            if !log.contains("distinct states") {
                return problem("exit 0, but no state count in the log: nothing was checked");
            }
            OutputCheck::default()
        }
        "full" => match parse_results(log) {
            (Some(v), Some(0)) if v > 0 => OutputCheck::default(),
            _ => problem("exit 0, but Verus verified nothing in this run"),
        },
        "mutants" => {
            let mut baseline = 0;
            let mut first_refusal_before_baseline = false;
            for l in log.lines() {
                let t = l.trim_start();
                if t == "baseline: verifies" {
                    baseline += 1;
                } else if (t.starts_with("refused") || t.starts_with("NOT REFUSED"))
                    && baseline == 0
                {
                    first_refusal_before_baseline = true;
                }
            }
            if baseline != 1 || first_refusal_before_baseline {
                return problem(&format!(
                    "the unmutated files were not shown to verify first (`baseline: verifies` seen {baseline} times, mutants judged before it: {first_refusal_before_baseline})"
                ));
            }
            let summary = log.lines().rev().find(|l| l.starts_with("mutants: "));
            let weak = summary.and_then(|l| {
                let (_, rest) = l.split_once("refused by verification, ")?;
                rest.split_whitespace().next()?.parse::<u64>().ok()
            });
            if weak.is_none() {
                return problem(
                    "no closing `mutants: N refused by verification, W refused without a verification run` line",
                );
            }
            OutputCheck {
                problem: None,
                weak,
            }
        }
        _ => OutputCheck::default(),
    }
}

/// `verification results:: 12 verified, 0 errors` -> (12, 0).
pub fn parse_results(log: &str) -> (Option<u64>, Option<u64>) {
    let mut out = (None, None);
    for line in log.lines() {
        if let Some(i) = line.find("verification results::") {
            let rest = &line[i + "verification results::".len()..];
            let nums: Vec<u64> = rest
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse().ok())
                .collect();
            if nums.len() >= 2 {
                // A file with several crates' results would sum; here each unit is one crate.
                out = (Some(nums[0]), Some(nums[1]));
            }
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeVerdict {
    Pass,
    /// Still queued or running: not a failure, but not a pass.
    Pending(String),
    Refuse(String),
}

/// One check run: name, status, conclusion, app slug. Newest first, as the checks API returns them for one name.
#[derive(Clone)]
pub struct CheckRun {
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub app: String,
}

/// May this commit merge? Only `success` from GitHub Actions passes; anything else, a missing check included, does not. A
/// `skipped` or `neutral` required check passes GitHub's own branch protection, so it is refused here by name.
pub fn merge_verdict(runs: &[CheckRun], required: &[String]) -> MergeVerdict {
    let mut pending = Vec::new();
    for name in required {
        let Some(r) = runs
            .iter()
            .find(|r| &r.name == name && r.app == "github-actions")
        else {
            return MergeVerdict::Refuse(format!(
                "{name}: no result for this commit (push the branch and let CI run)"
            ));
        };
        if r.status != "completed" {
            pending.push(format!("{name}: {}", r.status));
        } else if r.conclusion != "success" {
            return MergeVerdict::Refuse(format!("{name}: {}", r.conclusion));
        }
    }
    if pending.is_empty() {
        MergeVerdict::Pass
    } else {
        MergeVerdict::Pending(pending.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(kind: &'static str, exit_code: i32, oom: u64, timed: bool, v: Option<u64>) -> Outcome {
        classify(&Facts {
            kind,
            exit_code,
            oom_kills: oom,
            timed_out: timed,
            verified: v,
            errors: v.map(|_| 0),
        })
    }

    #[test]
    fn a_proof_needs_exit_zero_and_something_verified() {
        assert_eq!(facts("verus", 0, 0, false, Some(3)), Outcome::Verified);
        assert_eq!(facts("verus", 1, 0, false, Some(3)), Outcome::ProofFailed);
        assert_eq!(facts("verus", 0, 0, false, Some(0)), Outcome::Vacuous);
        assert_eq!(facts("verus", 0, 0, false, None), Outcome::Vacuous);
    }

    #[test]
    fn errors_in_the_log_fail_even_with_exit_zero() {
        let f = Facts {
            kind: "verus",
            exit_code: 0,
            oom_kills: 0,
            timed_out: false,
            verified: Some(3),
            errors: Some(1),
        };
        assert_eq!(classify(&f), Outcome::ProofFailed);
    }

    #[test]
    fn oom_is_not_a_refused_mutant_and_not_a_proof_failure() {
        assert_eq!(facts("mutants", 1, 1, false, None), Outcome::Oom);
        assert_eq!(facts("mutants", 0, 1, false, None), Outcome::Oom);
        assert_eq!(facts("verus", 137, 1, false, None), Outcome::Oom);
    }

    #[test]
    fn timeout_beats_everything() {
        assert_eq!(facts("verus", 0, 1, true, Some(3)), Outcome::Timeout);
        assert_eq!(facts("mutants", 1, 0, true, None), Outcome::Timeout);
    }

    #[test]
    fn a_failing_mutant_script_is_a_gate_failure() {
        assert_eq!(facts("mutants", 0, 0, false, None), Outcome::Verified);
        assert_eq!(facts("mutants", 1, 0, false, None), Outcome::MutantSurvived);
    }

    #[test]
    fn exit_codes_tell_oom_from_proof_failure() {
        assert_ne!(Outcome::Oom.exit_code(), Outcome::ProofFailed.exit_code());
        assert_ne!(
            Outcome::Timeout.exit_code(),
            Outcome::ProofFailed.exit_code()
        );
    }

    #[test]
    fn a_mutants_log_must_show_the_baseline_first() {
        let good = "baseline: verifies\nrefused: m1\nrefused (NO VERIFICATION RUN: compile or other error): m2\nmutants: 1 refused by verification, 1 refused without a verification run\n";
        let c = check_output("mutants", good);
        assert_eq!(
            c,
            OutputCheck {
                problem: None,
                weak: Some(1)
            }
        );
        // No baseline, or two, or a mutant judged before it, or no closing line: not evidence.
        for bad in [
            "refused: m1\nmutants: 1 refused by verification, 0 refused without a verification run\n",
            "baseline: verifies\nbaseline: verifies\nrefused: m\nmutants: 1 refused by verification, 0 refused without a verification run\n",
            "refused: m\nbaseline: verifies\nmutants: 1 refused by verification, 0 refused without a verification run\n",
            "baseline: verifies\nrefused: m1\n",
        ] {
            assert!(check_output("mutants", bad).problem.is_some(), "{bad}");
        }
    }

    #[test]
    fn a_pass_on_exit_code_alone_is_not_evidence_for_models_tla_deep_and_full() {
        assert!(
            check_output("models", "test result: ok. 8 passed; 0 failed\n")
                .problem
                .is_none()
        );
        assert!(
            check_output("models", "test result: ok. 0 passed; 0 failed\n")
                .problem
                .is_some()
        );
        assert!(
            check_output(
                "models",
                "test result: ok. 8 passed\ntest result: FAILED. 1 failed\n"
            )
            .problem
            .is_some()
        );
        assert!(check_output("models", "").problem.is_some());
        assert!(
            check_output("tla", "ok    A (tla-mc: pass, 5 distinct states found)\n")
                .problem
                .is_none()
        );
        assert!(
            check_output(
                "tla",
                "ok    A (tla-mc: pass, 5 distinct states found)\nFAIL  B (x)\n"
            )
            .problem
            .is_some()
        );
        assert!(check_output("tla", "nothing\n").problem.is_some());
        assert!(
            check_output("deep", "53613540 distinct states found\n")
                .problem
                .is_none()
        );
        assert!(check_output("deep", "done\n").problem.is_some());
        assert!(
            check_output("full", "verification results:: 5 verified, 0 errors\n")
                .problem
                .is_none()
        );
        assert!(
            check_output("full", "verification results:: 0 verified, 0 errors\n")
                .problem
                .is_some()
        );
        assert!(check_output("full", "").problem.is_some());
        assert!(
            check_output("verus", "").problem.is_none(),
            "other kinds are judged by classify"
        );
    }

    #[test]
    fn results_line_parses() {
        assert_eq!(
            parse_results("x\nverification results:: 12 verified, 0 errors\n"),
            (Some(12), Some(0))
        );
        assert_eq!(parse_results("error: nope"), (None, None));
    }

    fn run(name: &str, status: &str, conclusion: &str) -> CheckRun {
        CheckRun {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.into(),
            app: "github-actions".into(),
        }
    }

    fn req() -> Vec<String> {
        vec!["verus-gate".to_string()]
    }

    #[test]
    fn only_success_passes() {
        assert_eq!(
            merge_verdict(&[run("verus-gate", "completed", "success")], &req()),
            MergeVerdict::Pass
        );
        for c in [
            "failure",
            "cancelled",
            "timed_out",
            "skipped",
            "neutral",
            "action_required",
            "stale",
            "",
        ] {
            assert!(
                matches!(
                    merge_verdict(&[run("verus-gate", "completed", c)], &req()),
                    MergeVerdict::Refuse(_)
                ),
                "{c}"
            );
        }
    }

    #[test]
    fn missing_check_refuses() {
        assert!(matches!(
            merge_verdict(&[], &req()),
            MergeVerdict::Refuse(_)
        ));
        assert!(matches!(
            merge_verdict(&[run("other", "completed", "success")], &req()),
            MergeVerdict::Refuse(_)
        ));
    }

    #[test]
    fn queued_and_running_do_not_pass() {
        for s in ["queued", "in_progress", "waiting", "pending"] {
            assert!(
                matches!(
                    merge_verdict(&[run("verus-gate", s, "")], &req()),
                    MergeVerdict::Pending(_)
                ),
                "{s}"
            );
        }
    }

    #[test]
    fn newest_run_decides() {
        let runs = [
            run("verus-gate", "completed", "failure"),
            run("verus-gate", "completed", "success"),
        ];
        assert!(matches!(
            merge_verdict(&runs, &req()),
            MergeVerdict::Refuse(_)
        ));
    }

    #[test]
    fn a_status_from_another_app_is_not_trusted() {
        let mut r = run("verus-gate", "completed", "success");
        r.app = "someone-else".into();
        assert!(matches!(
            merge_verdict(&[r], &req()),
            MergeVerdict::Refuse(_)
        ));
    }
}
