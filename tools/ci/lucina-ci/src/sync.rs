//! Pure decisions for the automatic public sync and for "which commit is proven" (docs/ci/PUBLIC-SYNC.md). No process, no network:
//! the commands in main.rs gather the facts and act on what these return.

use crate::verdict::{CheckRun, MergeVerdict, merge_verdict};

/// `Sync from LucinaOS main 1a2b3c4` -> `1a2b3c4`. Only the first line counts, and only a hex word.
pub fn parse_sync_subject(message: &str) -> Option<String> {
    let first = message.lines().next()?.trim();
    let rest = first.strip_prefix("Sync from LucinaOS main ")?.trim();
    let ok = (7..=40).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| rest.to_ascii_lowercase())
}

/// How the last attempt at a private commit ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Last {
    /// Public `main` took it.
    Done,
    /// A scan finding or a red gate stopped it. Not retried: a person looks at it.
    Failed,
    /// Started and never finished (a crash). Retried once more, because nothing was decided.
    Started,
    /// Transient trouble outlasted the retries (epoch seconds of the alert). Not a verdict on the commit: tried again once an hour.
    Stalled(u64),
}

/// How long a stalled tip waits before the next round of retries.
pub const STALL_WAIT_SECS: u64 = 3600;

/// `state` file text: `<sha> <done|failed|started>` or `<sha> stalled <epoch>`.
pub fn parse_state(text: &str) -> Option<(String, Last)> {
    let mut w = text.split_whitespace();
    let sha = w.next()?.to_string();
    let last = match w.next()? {
        "done" => Last::Done,
        "failed" => Last::Failed,
        "started" => Last::Started,
        "stalled" => Last::Stalled(w.next()?.parse().ok()?),
        _ => return None,
    };
    Some((sha, last))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Sync this private commit (always the newest main: older ones are coalesced into it).
    Sync(String),
    /// Nothing to do, and why.
    Skip(&'static str),
}

/// What to do now that private `main` is at `main_sha`. One sync per tip, never one per commit, never the same failed tip twice.
pub fn plan(main_sha: &str, last: Option<&(String, Last)>, now: u64) -> Plan {
    match last {
        Some((sha, Last::Stalled(at)))
            if sha == main_sha && now < at.saturating_add(STALL_WAIT_SECS) =>
        {
            Plan::Skip(
                "this main is stalled on transient trouble; the next round of retries is within the hour",
            )
        }
        Some((sha, Last::Done)) if sha == main_sha => {
            Plan::Skip("main is already on the public mirror")
        }
        Some((sha, Last::Failed)) if sha == main_sha => {
            Plan::Skip("this main failed its last sync and waits for a person (no retry loop)")
        }
        _ => Plan::Sync(main_sha.to_string()),
    }
}

/// What the gate poll on the `sync/` branch's commit means.
#[derive(Debug, PartialEq, Eq)]
pub enum GateStep {
    Wait(String),
    /// Both gates are `success`: `main` may take the commit (if the push is a fast-forward).
    Promote,
    Fail(String),
    /// A required check never appeared within the grace period (a runner that never started): not a verdict on the commit.
    Stuck(String),
}

/// How long after the push a check that has not appeared yet still counts as "not started" and not as "missing". GitHub registers
/// the runs a minute or more after the push (seen 2026-11-11: the first poll, 60 s after the push, found none).
pub const START_GRACE_SECS: u64 = 15 * 60;

/// `since_push_secs`: time since the `sync/` branch was pushed. A red gate fails at once; a gate with no run yet waits inside the
/// grace period and is a failure after it.
pub fn gate_step(runs: &[CheckRun], required: &[String], since_push_secs: u64) -> GateStep {
    let present = |n: &String| {
        runs.iter()
            .any(|r| &r.name == n && r.app == "github-actions")
    };
    let missing: Vec<&String> = required.iter().filter(|n| !present(n)).collect();
    if !missing.is_empty() && since_push_secs < START_GRACE_SECS {
        let seen: Vec<String> = required.iter().filter(|n| present(n)).cloned().collect();
        if let MergeVerdict::Refuse(w) = merge_verdict(runs, &seen) {
            return GateStep::Fail(w);
        }
        let names: Vec<&str> = missing.iter().map(|n| n.as_str()).collect();
        return GateStep::Wait(format!("{}: not started yet", names.join(", ")));
    }
    if !missing.is_empty() {
        let seen: Vec<String> = required.iter().filter(|n| present(n)).cloned().collect();
        if let MergeVerdict::Refuse(w) = merge_verdict(runs, &seen) {
            return GateStep::Fail(w);
        }
        let names: Vec<&str> = missing.iter().map(|n| n.as_str()).collect();
        return GateStep::Stuck(format!("{}: never started", names.join(", ")));
    }
    match merge_verdict(runs, required) {
        MergeVerdict::Pass => GateStep::Promote,
        MergeVerdict::Pending(w) => GateStep::Wait(w),
        MergeVerdict::Refuse(w) => GateStep::Fail(w),
    }
}

/// The last check before `main` moves. A scan finding, a non-fast-forward or anything but two green gates stops it.
pub fn may_move_main(scan_clean: bool, gates: &GateStep, fast_forward: bool) -> Result<(), String> {
    if !scan_clean {
        return Err("a scan finding".into());
    }
    if !fast_forward {
        return Err("the push would not be a fast-forward (never forced)".into());
    }
    match gates {
        GateStep::Promote => Ok(()),
        GateStep::Wait(w) => Err(format!("gates not finished: {w}")),
        GateStep::Fail(w) => Err(format!("gate not green: {w}")),
        GateStep::Stuck(w) => Err(format!("gate never ran: {w}")),
    }
}

/// One public commit as GitHub reports it: its message and its check runs.
pub struct PublicCommit {
    pub sha: String,
    pub message: String,
    pub runs: Vec<CheckRun>,
}

fn is_deep(r: &CheckRun) -> bool {
    r.app == "github-actions" && r.name.contains("(deep)")
}

/// Proven = both gates `success`, and every deep-tier job that ran on it `success` (a deep tier that never ran does not block: it is
/// on demand, nightly and after a sync). A deep job still running, or failed, does not count as proven.
pub fn is_proven(runs: &[CheckRun], required: &[String]) -> bool {
    if merge_verdict(runs, required) != MergeVerdict::Pass {
        return false;
    }
    let mut seen: Vec<&str> = Vec::new();
    for r in runs.iter().filter(|r| is_deep(r)) {
        // Newest first per name: only the first of each name decides.
        if seen.contains(&r.name.as_str()) {
            continue;
        }
        seen.push(&r.name);
        if r.status != "completed" || r.conclusion != "success" {
            return false;
        }
    }
    true
}

/// The newest private commit whose public sync is proven. `commits` is newest first (public `main`'s history). `resolve` turns the
/// short sha in the message into a full private sha (None: not in the private repository, so it is skipped).
/// With `only`, only that private commit may be the answer (the install gate's "is this one proven?").
pub fn newest_proven(
    commits: &[PublicCommit],
    required: &[String],
    only: Option<&str>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    for c in commits {
        let Some(short) = parse_sync_subject(&c.message) else {
            continue;
        };
        let Some(full) = resolve(&short) else {
            continue;
        };
        if only.is_some_and(|o| o != full) {
            continue;
        }
        if is_proven(&c.runs, required) {
            return Some((full, c.sha.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, status: &str, conclusion: &str) -> CheckRun {
        CheckRun {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.into(),
            app: "github-actions".into(),
        }
    }
    fn req() -> Vec<String> {
        vec!["verus-gate".into(), "rust-gate".into()]
    }
    fn green() -> Vec<CheckRun> {
        vec![
            run("verus-gate", "completed", "success"),
            run("rust-gate", "completed", "success"),
        ]
    }

    #[test]
    fn subject_parses_only_the_sync_line() {
        assert_eq!(
            parse_sync_subject("Sync from LucinaOS main 82E18E4\n\nbody"),
            Some("82e18e4".into())
        );
        assert_eq!(parse_sync_subject("Sync from LucinaOS main xyz1234"), None);
        assert_eq!(parse_sync_subject("Sync from LucinaOS main abc"), None);
        assert_eq!(
            parse_sync_subject("fix: Sync from LucinaOS main 82e18e4"),
            None
        );
        assert_eq!(parse_sync_subject(""), None);
    }

    #[test]
    fn state_parses() {
        assert_eq!(
            parse_state("abc1234 done\n"),
            Some(("abc1234".into(), Last::Done))
        );
        assert_eq!(
            parse_state("abc1234 failed"),
            Some(("abc1234".into(), Last::Failed))
        );
        assert_eq!(parse_state("abc1234 maybe"), None);
        assert_eq!(parse_state(""), None);
    }

    #[test]
    fn plan_coalesces_and_does_not_loop() {
        assert_eq!(plan("b", None, 0), Plan::Sync("b".into()));
        // Newer main than the last attempt: one sync of the tip, whatever lay between.
        assert_eq!(
            plan("c", Some(&("a".into(), Last::Done)), 0),
            Plan::Sync("c".into())
        );
        assert_eq!(
            plan("c", Some(&("a".into(), Last::Failed)), 0),
            Plan::Sync("c".into())
        );
        assert!(matches!(
            plan("a", Some(&("a".into(), Last::Done)), 0),
            Plan::Skip(_)
        ));
        assert!(matches!(
            plan("a", Some(&("a".into(), Last::Failed)), 0),
            Plan::Skip(_)
        ));
        // A crash mid-run decided nothing: try again.
        assert_eq!(
            plan("a", Some(&("a".into(), Last::Started)), 0),
            Plan::Sync("a".into())
        );
    }

    #[test]
    fn stalled_tips_wait_an_hour_and_then_retry() {
        let st = Some(("a".to_string(), Last::Stalled(1000)));
        assert!(matches!(plan("a", st.as_ref(), 1000 + 60), Plan::Skip(_)));
        assert_eq!(
            plan("a", st.as_ref(), 1000 + STALL_WAIT_SECS),
            Plan::Sync("a".into())
        );
        // A newer main starts at once.
        assert_eq!(plan("b", st.as_ref(), 1001), Plan::Sync("b".into()));
        assert_eq!(
            parse_state("abc1234 stalled 1700000000"),
            Some(("abc1234".into(), Last::Stalled(1700000000)))
        );
        assert_eq!(parse_state("abc1234 stalled"), None);
    }

    #[test]
    fn gate_step_maps_the_verdict() {
        assert_eq!(gate_step(&green(), &req(), 60), GateStep::Promote);
        assert!(matches!(
            gate_step(
                &[
                    run("verus-gate", "completed", "success"),
                    run("rust-gate", "in_progress", "")
                ],
                &req(),
                60
            ),
            GateStep::Wait(_)
        ));
        assert!(matches!(
            gate_step(
                &[
                    run("verus-gate", "completed", "failure"),
                    run("rust-gate", "completed", "success")
                ],
                &req(),
                60
            ),
            GateStep::Fail(_)
        ));
        assert!(matches!(
            gate_step(&[], &req(), START_GRACE_SECS),
            GateStep::Stuck(_)
        ));
    }

    #[test]
    fn checks_that_have_not_started_wait_inside_the_grace_period() {
        assert!(matches!(gate_step(&[], &req(), 60), GateStep::Wait(_)));
        let one = [run("verus-gate", "completed", "success")];
        assert!(matches!(gate_step(&one, &req(), 60), GateStep::Wait(_)));
        assert!(matches!(
            gate_step(&one, &req(), START_GRACE_SECS),
            GateStep::Stuck(_)
        ));
        // After the grace period a red gate is still a failure of the commit, not a stall.
        let red_late = [run("verus-gate", "completed", "failure")];
        assert!(matches!(
            gate_step(&red_late, &req(), START_GRACE_SECS),
            GateStep::Fail(_)
        ));
        // A red gate does not wait for the other one.
        let red = [run("verus-gate", "completed", "failure")];
        assert!(matches!(gate_step(&red, &req(), 60), GateStep::Fail(_)));
        let queued = [run("verus-gate", "queued", "")];
        assert!(matches!(gate_step(&queued, &req(), 60), GateStep::Wait(_)));
    }

    #[test]
    fn main_moves_only_when_everything_holds() {
        assert!(may_move_main(true, &GateStep::Promote, true).is_ok());
        assert!(may_move_main(false, &GateStep::Promote, true).is_err());
        assert!(may_move_main(true, &GateStep::Promote, false).is_err());
        assert!(may_move_main(true, &GateStep::Wait("x".into()), true).is_err());
        assert!(may_move_main(true, &GateStep::Fail("x".into()), true).is_err());
    }

    #[test]
    fn deep_jobs_that_ran_must_be_green() {
        let mut r = green();
        assert!(is_proven(&r, &req()));
        r.push(run("g (deep) JunoIdleWake3", "completed", "success"));
        assert!(is_proven(&r, &req()));
        let mut bad = green();
        bad.push(run("g (deep) JunoIdleWake3", "completed", "failure"));
        assert!(!is_proven(&bad, &req()));
        let mut running = green();
        running.push(run("g (deep) JunoIdleWake3", "in_progress", ""));
        assert!(!is_proven(&running, &req()));
        // Newest first: a green re-run after a red one is what counts.
        let mut rerun = green();
        rerun.push(run("g (deep) X", "completed", "success"));
        rerun.push(run("g (deep) X", "completed", "failure"));
        assert!(is_proven(&rerun, &req()));
    }

    #[test]
    fn a_red_or_missing_gate_is_not_proven() {
        assert!(!is_proven(&[], &req()));
        assert!(!is_proven(
            &[run("verus-gate", "completed", "success")],
            &req()
        ));
    }

    fn pc(sha: &str, msg: &str, runs: Vec<CheckRun>) -> PublicCommit {
        PublicCommit {
            sha: sha.into(),
            message: msg.into(),
            runs,
        }
    }

    #[test]
    fn newest_proven_skips_red_and_unknown_commits() {
        let resolve = |s: &str| match s {
            "aaaaaaa" => Some("a".repeat(40)),
            "bbbbbbb" => Some("b".repeat(40)),
            _ => None,
        };
        let commits = vec![
            pc("p3", "Sync from LucinaOS main ccccccc", green()), // not in the private repo
            pc(
                "p2",
                "Sync from LucinaOS main bbbbbbb",
                vec![run("verus-gate", "completed", "failure")],
            ),
            pc("p1", "Sync from LucinaOS main aaaaaaa", green()),
            pc("p0", "Initial import", green()),
        ];
        assert_eq!(
            newest_proven(&commits, &req(), None, &resolve),
            Some(("a".repeat(40), "p1".into()))
        );
        assert_eq!(
            newest_proven(&commits, &req(), Some(&"a".repeat(40)), &resolve),
            Some(("a".repeat(40), "p1".into()))
        );
        assert_eq!(
            newest_proven(&commits, &req(), Some(&"b".repeat(40)), &resolve),
            None
        );
        assert_eq!(newest_proven(&[], &req(), None, &resolve), None);
    }
}
