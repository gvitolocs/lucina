//! `check-fast --merge-gate` forwards the merge job's waivers to diffgate (finding GATE-01: `lagent merge --allow rule=reason`
//! waived the refusals of the merge gate's own diffgate pass, and the `diffgate` step of the fast check then refused those
//! very lines again with "5 refusal(s), 0 waived" — the real log is job_4ee819df2427, its line 9 the allow it was given).
//!
//! The real binary runs over a throwaway repository holding a byte-for-byte copy of its own sources (so its source-id check
//! passes), with a stub `diffgate` first on PATH that writes the arguments it was called with, one per line, to a file. What
//! reached diffgate is the proof: the step runs its command through `bash -c`, so the reason has to survive the shell on the
//! way. A run that left no file at all is a run in which diffgate never started, so nothing was waived.
//!
//! Every run here is `--no-build`, which keeps a run to a fraction of a second. The merge gate still refuses the inventory,
//! spec, roles and trust steps of a repository that holds no `tools/ci/proof-inventory.tsv`, so these tests judge the
//! `diffgate` step's own line and never the exit code.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

/// The rules the step forwards, read from the real list: the list is data, and a rule added to it is forwarded by the
/// same code these tests exercise.
const FORWARD_LIST: &str = "tools/ci/merge-waiver-rules.txt";

/// The allow of job_4ee819df2427 line 9, as its coordinator wrote it — the reason the step refused anyway (line 160:
/// `diffgate: 5 refusal(s), 0 waived, 7 files, +1617/-244 lines`).
const GAL_REASON: &str = "real-server variants kept as #[ignore = needs LUCINA_MEDIA_BIN], reported ignored not passed; \
                         the same assertions run against the in-process Fake; the ignored variants run in a scheduled \
                         non-gating job; WM-C004 / GAL-01; coordinator allow 2026-11-17";

struct Case {
    d: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
    /// What the stub `diffgate` was called with, or no file at all when it never ran.
    argv: PathBuf,
    waivers: PathBuf,
    base: String,
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy_dir(&p, &q);
        } else {
            fs::copy(&p, &q).unwrap();
        }
    }
}

/// git, with no inherited config: `bq` takes `GIT_CONFIG_KEY_0` out of a job's environment (its name holds KEY) and leaves
/// `GIT_CONFIG_COUNT` behind, which a git that is asked to read one config line cannot do. A throwaway repository is built
/// the same way whatever the caller's shell carries.
fn git(d: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(d)
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_KEY_0")
        .env_remove("GIT_CONFIG_VALUE_0")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// A stub goes in through a child, never from this thread: it is exec'd by the step, and a file this thread still holds open
/// for writing makes that exec answer "Text file busy" (ETXTBSY) when another thread forks at the same moment, after which
/// the shell goes on to the next `diffgate` on PATH, the real one. The same helper in SecretaryIOS explains the measurement.
fn write_exec(path: &Path, script: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    w.write_all(script.as_bytes()).unwrap();
    drop(w);
    assert!(
        child.wait().unwrap().success(),
        "sh cannot write {}: the stand-in is never exec'd",
        path.display()
    );
}

fn stub(dir: &Path, name: &str, body: &str) {
    write_exec(&dir.join(name), &format!("#!/bin/sh\n{body}\n"));
}

impl Case {
    /// A repository whose `main` is empty and whose `work` branch holds this crate, and a PATH of stub tools in front of it.
    /// The folder goes when the test ends, panic included.
    fn setup(tag: &str) -> Case {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("mgw-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = d.join("repo");
        let dst = repo.join("tools/ci/lucina-ci");
        fs::create_dir_all(&dst).unwrap();
        copy_dir(&crate_dir.join("src"), &dst.join("src"));
        for f in ["Cargo.toml", "Cargo.lock"] {
            fs::copy(crate_dir.join(f), dst.join(f)).unwrap();
        }
        // The step reads its forwarding list from the repository under test, so the throwaway one holds the real file:
        // what these tests forward is what the owner listed, and a repository without it forwards nothing.
        fs::copy(repo_root().join(FORWARD_LIST), repo.join(FORWARD_LIST)).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &["commit", "-q", "--allow-empty", "-m", "empty base"],
        );
        git(&repo, &["checkout", "-q", "-b", "work"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "the crate"]);
        let base = git(&repo, &["rev-parse", "main"]);
        let bin = d.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let (argv, waivers) = (bin.join("diffgate.argv"), d.join("waivers.jsonl"));
        stub(
            &bin,
            "diffgate",
            &format!("printf '%s\\n' \"$@\" > '{}'\nexit 0", argv.display()),
        );
        stub(&bin, "gitleaks", "exit 0");
        Case {
            d,
            repo,
            bin,
            argv,
            waivers,
            base,
        }
    }

    fn write_lines(&self, lines: &[String]) {
        fs::write(&self.waivers, format!("{}\n", lines.join("\n"))).unwrap();
    }

    /// `Some` is `LAGENT_WAIVERS_FILE`; `None` takes the variable off the child's environment, so whatever the caller's
    /// shell holds cannot decide the test.
    fn run(&self, waivers: Option<&Path>) -> String {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
        c.args([
            "check-fast",
            "--base",
            "main",
            "--merge-gate",
            "--no-build",
            "--mem-cap-gib",
            "1",
        ])
        .current_dir(&self.repo)
        .env("PATH", path)
        .env_remove("LUCINA_CHECK_FAST_MERGE_CAP_S");
        // `bq` leaves GIT_CONFIG_COUNT behind without its key: the step's own git calls, and the real diffgate it runs, must not
        // inherit it.
        for n in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
            c.env_remove(n);
        }
        match waivers {
            Some(p) => c.env("LAGENT_WAIVERS_FILE", p),
            None => c.env_remove("LAGENT_WAIVERS_FILE"),
        };
        let o = c.output().unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    }

    fn argv(&self) -> Option<Vec<String>> {
        fs::read_to_string(&self.argv)
            .ok()
            .map(|t| t.lines().map(str::to_string).collect())
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.d);
    }
}

fn step_line<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no line for step {name}:\n{out}"))
}

/// What the step runs today, with no waiver of any kind.
fn plain(base: &str) -> Vec<String> {
    ["--repo", ".", "--base", base]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The same call with one `--allow` per entry of `allows`, in order.
fn forwarded(base: &str, allows: &[&str]) -> Vec<String> {
    let mut v = plain(base);
    for a in allows {
        v.push("--allow".into());
        v.push((*a).to_string());
    }
    v
}

/// One waivers line as the merge job writes it, the reason JSON-escaped the way its writer escapes it.
fn wline(rule: &str, reason: &str) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"rule\":\"{}\",\"reason\":\"{}\",\"by\":\"lead\",\"job\":\"job_4ee819df2427\"}}",
        esc(rule),
        esc(reason)
    )
}

/// The repository this test file sits in: the forwarding list is the real one, not a copy written here.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The repo's own forwarding list, as the step reads it: the rule name of each line, its marker left out.
fn the_list() -> Vec<String> {
    fs::read_to_string(repo_root().join(FORWARD_LIST))
        .unwrap_or_else(|e| panic!("{} is not readable from the test: {e}", FORWARD_LIST))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// The rules the list marks `counted`, whose waivers travel only as `N:reason`.
fn counted_rules() -> Vec<String> {
    fs::read_to_string(repo_root().join(FORWARD_LIST))
        .unwrap_or_else(|e| panic!("{} is not readable from the test: {e}", FORWARD_LIST))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.split_whitespace().nth(1) == Some("counted"))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// `LAGENT_WAIVERS_FILE` not set — every run that is not a merge with an allow. The command is the plain one, and the
/// output says nothing about waivers.
#[test]
fn no_waivers_file_leaves_the_diffgate_step_exactly_as_it_was() {
    if common::skip_without_scope("no_waivers_file_leaves_the_diffgate_step_exactly_as_it_was") {
        return;
    }
    let c = Case::setup("unset");
    let out = c.run(None);
    assert_eq!(
        c.argv().expect("diffgate ran"),
        plain(&c.base),
        "the step calls diffgate as it always did"
    );
    assert!(
        step_line(&out, "diffgate").starts_with("ok"),
        "{}",
        step_line(&out, "diffgate")
    );
    assert!(
        !out.contains("waiver") && !out.contains("--allow"),
        "an ordinary run says nothing about waivers:\n{out}"
    );
}

/// The counted form of diffgate (`RULE=N:reason`) reaches it byte for byte through the shell: quotes, the closing bracket
/// of the attribute and the semicolons of the reason are diffgate's to read, not the shell's.
#[test]
fn a_counted_waiver_of_a_listed_rule_reaches_diffgate_byte_for_byte() {
    if common::skip_without_scope(
        "a_counted_waiver_of_a_listed_rule_reaches_diffgate_byte_for_byte",
    ) {
        return;
    }
    let c = Case::setup("counted");
    let reason = "7:seven tests are #[ignore = \"slow\"]; see a/b";
    c.write_lines(&[wline("skipped-test", reason)]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(
            &c.base,
            &["skipped-test=7:seven tests are #[ignore = \"slow\"]; see a/b"]
        ),
        "the whole allow is one argument, as written\n{out}"
    );
    assert!(
        out.contains("diffgate waivers: 1 forwarded (skipped-test: 7:seven tests are #[ignore = \"slow\"]; see a/b), 0 not forwarded"),
        "the step says what it forwarded:\n{out}"
    );
}

/// The uncounted reason of the merge that failed, line 9 of job_4ee819df2427, forwarded as it stands.
#[test]
fn an_uncounted_waiver_of_a_listed_rule_reaches_diffgate_unchanged() {
    if common::skip_without_scope("an_uncounted_waiver_of_a_listed_rule_reaches_diffgate_unchanged")
    {
        return;
    }
    let c = Case::setup("uncounted");
    c.write_lines(&[wline("skipped-test", GAL_REASON)]);
    let out = c.run(Some(&c.waivers));
    let want = format!("skipped-test={GAL_REASON}");
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[want.as_str()])
    );
    assert!(
        step_line(&out, "diffgate").starts_with("ok"),
        "{}",
        step_line(&out, "diffgate")
    );
}

/// A reason is free text a human typed: an apostrophe, a command substitution and a redirect are content, never the
/// shell's instructions.
#[test]
fn a_reason_holding_quotes_and_a_command_is_one_unchanged_argument() {
    if common::skip_without_scope("a_reason_holding_quotes_and_a_command_is_one_unchanged_argument")
    {
        return;
    }
    let c = Case::setup("quoted");
    let marker = c.d.join("it-ran");
    let reason = format!(
        "it's the coordinator's word; touch '{}'  $(echo hi) `echo there` \"slow\"",
        marker.display()
    );
    c.write_lines(&[wline("skipped-test", &reason)]);
    let out = c.run(Some(&c.waivers));
    let want = format!("skipped-test={reason}");
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[want.as_str()]),
        "the reason is one argument, byte for byte:\n{out}"
    );
    assert!(
        !marker.exists(),
        "the reason ran a command through the step's shell:\n{out}"
    );
}

/// Every rule of the list travels: today it holds `skipped-test` and `test-removed`, and a rule added there is forwarded by this same
/// path (adding it is a gate change the reviewer reads).
#[test]
fn every_rule_of_the_forwarding_list_is_forwarded_to_diffgate() {
    if common::skip_without_scope("every_rule_of_the_forwarding_list_is_forwarded_to_diffgate") {
        return;
    }
    let list = the_list();
    assert_eq!(
        list,
        vec!["skipped-test".to_string(), "test-removed".to_string()],
        "the list today"
    );
    let c = Case::setup("list");
    let lines: Vec<String> = list
        .iter()
        .map(|r| wline(r, "5:five of them; the coordinator allow of today"))
        .collect();
    let allows: Vec<String> = list
        .iter()
        .map(|r| format!("{r}=5:five of them; the coordinator allow of today"))
        .collect();
    c.write_lines(&lines);
    let out = c.run(Some(&c.waivers));
    let mut want = plain(&c.base);
    for a in &allows {
        want.push("--allow".into());
        want.push(a.clone());
    }
    assert_eq!(
        c.argv().expect("diffgate ran"),
        want,
        "one --allow per listed rule\n{out}"
    );
}

/// `gate-change` and `spec-change` are the merge tool's own decisions, not rules diffgate knows: forwarding one would fail
/// the call with "--allow: unknown rule", so they stay out of the command and the line says they were left.
#[test]
fn the_merge_tool_s_own_rules_are_not_forwarded() {
    if common::skip_without_scope("the_merge_tool_s_own_rules_are_not_forwarded") {
        return;
    }
    let c = Case::setup("not-forwarded");
    c.write_lines(&[
        wline("gate-change", "tools/ci/ of this merge; coordinator allow"),
        wline(
            "spec-change",
            "a statement moved with its proof; coordinator allow",
        ),
    ]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        plain(&c.base),
        "diffgate is called as if no allow had been given"
    );
    assert!(
        out.contains("diffgate waivers: 0 forwarded, 2 not forwarded (gate-change, spec-change)"),
        "the step says what it left:\n{out}"
    );
}

/// One line of the gate log, counting both sides.
#[test]
fn the_step_prints_one_line_about_the_waivers_it_was_given() {
    if common::skip_without_scope("the_step_prints_one_line_about_the_waivers_it_was_given") {
        return;
    }
    let c = Case::setup("summary");
    c.write_lines(&[
        wline("skipped-test", "3:three #[ignore] variants; WM-C004"),
        wline("gate-change", "tools/ci/ of this merge; coordinator allow"),
    ]);
    let out = c.run(Some(&c.waivers));
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("diffgate waivers: "))
        .collect();
    assert_eq!(
        lines,
        [
            "diffgate waivers: 1 forwarded (skipped-test: 3:three #[ignore] variants; WM-C004), 1 not forwarded (gate-change)"
        ],
        "exactly one line, and it counts both sides:\n{out}"
    );
}

/// A waivers file the step cannot read exactly is a refusal of its own: not a silent "nothing waived", and never a skip.
fn assert_refused(tag: &str, content: &str, says: &str) {
    let c = Case::setup(tag);
    fs::write(&c.waivers, content).unwrap();
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "the malformed file was not a refusal: {}\n{out}",
        step_line(&out, "diffgate")
    );
    assert!(out.contains(says), "nothing says {says:?}:\n{out}");
    assert!(
        !out.contains("diffgate waivers: 0 forwarded"),
        "a refusal is not reported as a step that waived nothing of a readable file:\n{out}"
    );
}

#[test]
fn a_waivers_file_that_cannot_be_read_fails_the_step() {
    if common::skip_without_scope("a_waivers_file_that_cannot_be_read_fails_the_step") {
        return;
    }
    let c = Case::setup("missing");
    let gone = c.d.join("no-such-dir").join("waivers.jsonl");
    let out = c.run(Some(&gone));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "a path that holds no file was not a refusal:\n{out}"
    );
    assert!(
        out.contains("cannot be read"),
        "the message does not say why:\n{out}"
    );
    assert!(
        c.argv().is_none(),
        "diffgate ran on a file that is not there"
    );
}

#[test]
fn an_empty_waivers_file_fails_the_step() {
    if common::skip_without_scope("an_empty_waivers_file_fails_the_step") {
        return;
    }
    assert_refused("empty", "", "holds no waiver line");
}

#[test]
fn a_line_that_is_not_json_fails_the_step() {
    if common::skip_without_scope("a_line_that_is_not_json_fails_the_step") {
        return;
    }
    assert_refused(
        "not-json",
        "the reason was written without braces\n",
        "line 1: not a JSON object",
    );
}

#[test]
fn a_line_missing_one_of_the_four_fields_fails_the_step() {
    if common::skip_without_scope("a_line_missing_one_of_the_four_fields_fails_the_step") {
        return;
    }
    assert_refused(
        "no-field",
        "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"lead\"}\n",
        "no `job` field",
    );
}

#[test]
fn a_field_that_is_not_a_string_fails_the_step() {
    if common::skip_without_scope("a_field_that_is_not_a_string_fails_the_step") {
        return;
    }
    assert_refused(
        "not-a-string",
        "{\"rule\":\"skipped-test\",\"reason\":7,\"by\":\"lead\",\"job\":\"job_4ee819df2427\"}\n",
        "the value of `reason` is not a string",
    );
}

#[test]
fn an_extra_field_fails_the_step() {
    if common::skip_without_scope("an_extra_field_fails_the_step") {
        return;
    }
    assert_refused(
        "extra-field",
        "{\"rule\":\"skipped-test\",\"reason\":\"why\",\"by\":\"lead\",\"job\":\"job_1\",\"who\":\"me\"}\n",
        "5 fields",
    );
}

#[test]
fn a_blank_reason_fails_the_step() {
    if common::skip_without_scope("a_blank_reason_fails_the_step") {
        return;
    }
    assert_refused(
        "blank-reason",
        "{\"rule\":\"skipped-test\",\"reason\":\"   \",\"by\":\"lead\",\"job\":\"job_1\"}\n",
        "`reason` is empty",
    );
}

/// The point of reading the file at all: a waiver this step cannot read waives NOTHING, not even the sound lines that
/// stand beside it. The good line is the one job_4ee819df2427 was given, so the refusal it would have waived is the
/// difference between this merge and the one that failed.
#[test]
fn one_bad_line_waives_nothing_even_beside_a_good_one() {
    if common::skip_without_scope("one_bad_line_waives_nothing_even_beside_a_good_one") {
        return;
    }
    let c = Case::setup("half-bad");
    c.write_lines(&[
        wline("skipped-test", GAL_REASON),
        "and then a line the merge job did not write".into(),
    ]);
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "the bad line was not a refusal:\n{out}"
    );
    assert!(
        c.argv().is_none(),
        "diffgate ran and waived from a file this step cannot read:\n{out}"
    );
}

/// `test-removed` is forwarded count-exact, as the list holds it: a merge given `--allow test-removed=2:<reason>` passes the
/// merge's own diffgate, and the fast check's `diffgate` step has to see the same allow. The whole counted allow reaches
/// diffgate unchanged (diffgate itself compares the count with the refusals), and the gate log carries the reason text.
#[test]
fn test_removed_is_on_the_list_as_counted_and_travels_with_its_reason() {
    if common::skip_without_scope(
        "test_removed_is_on_the_list_as_counted_and_travels_with_its_reason",
    ) {
        return;
    }
    assert!(
        counted_rules().iter().any(|r| r == "test-removed"),
        "{FORWARD_LIST} does not hold test-removed as a counted rule: {:?}",
        counted_rules()
    );
    let reason =
        "2:two obsolete tests deleted with the code they covered; coordinator allow of the day";
    let c = Case::setup("test-removed");
    c.write_lines(&[wline("test-removed", reason)]);
    let out = c.run(Some(&c.waivers));
    assert_eq!(
        c.argv().expect("diffgate ran"),
        forwarded(&c.base, &[&format!("test-removed={reason}")]),
        "the whole counted allow, unchanged, as one argument\n{out}"
    );
    assert!(
        out.contains(&format!(
            "diffgate waivers: 1 forwarded (test-removed: {reason}), 0 not forwarded"
        )),
        "the gate log carries the reason text:\n{out}"
    );
}

/// The counted form only: an uncounted `test-removed` waiver fails the step and diffgate never runs.
#[test]
fn an_uncounted_test_removed_waiver_fails_the_step() {
    if common::skip_without_scope("an_uncounted_test_removed_waiver_fails_the_step") {
        return;
    }
    let c = Case::setup("test-removed-uncounted");
    c.write_lines(&[wline("test-removed", "two obsolete tests deleted")]);
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "an uncounted test-removed waiver was not refused: {}\n{out}",
        step_line(&out, "diffgate")
    );
    assert!(
        out.contains("diffgate waivers: test-removed is forwarded in counted form only: write --allow test-removed=N:<reason> with N the exact number of refusals"),
        "the message does not say what to write:\n{out}"
    );
    assert!(c.argv().is_none(), "diffgate ran:\n{out}");
    assert!(!out.contains("forwarded ("), "nothing is forwarded:\n{out}");
}

/// `skipped-test` is a plain line: its uncounted reason (the allow of the merge that failed) and a counted one both travel.
#[test]
fn a_rule_the_list_leaves_plain_travels_in_both_forms() {
    if common::skip_without_scope("a_rule_the_list_leaves_plain_travels_in_both_forms") {
        return;
    }
    let marked = counted_rules();
    assert!(
        !marked.iter().any(|r| r == "skipped-test"),
        "{FORWARD_LIST} marks skipped-test counted: {marked:?}"
    );
    for (tag, reason) in [
        ("plain-uncounted", GAL_REASON),
        ("plain-counted", "3:three ignored tests, each one read"),
    ] {
        let c = Case::setup(tag);
        c.write_lines(&[wline("skipped-test", reason)]);
        let out = c.run(Some(&c.waivers));
        assert_eq!(
            c.argv().expect("diffgate ran"),
            forwarded(&c.base, &[&format!("skipped-test={reason}")]),
            "{reason} travels as it stands\n{out}"
        );
    }
}

/// A word after the rule name that is not `counted` is a marker this step cannot read: it fails the step and names the line,
/// rather than silently forwarding a rule that was meant to be restricted.
#[test]
fn an_unknown_marker_on_a_list_line_fails_the_step() {
    if common::skip_without_scope("an_unknown_marker_on_a_list_line_fails_the_step") {
        return;
    }
    let c = Case::setup("unknown-marker");
    fs::write(
        c.repo.join(FORWARD_LIST),
        "# a test-only list\nskipped-test\ntest-removed sometimes\n",
    )
    .unwrap();
    c.write_lines(&[wline("skipped-test", GAL_REASON)]);
    let out = c.run(Some(&c.waivers));
    assert!(
        step_line(&out, "diffgate").starts_with("FAIL"),
        "a marker the step does not know was not a refusal: {}\n{out}",
        step_line(&out, "diffgate")
    );
    assert!(
        out.contains("unknown marker \"sometimes\" after test-removed"),
        "the message does not name the word and the rule:\n{out}"
    );
    assert!(
        c.argv().is_none(),
        "diffgate ran on a list this step cannot read\n{out}"
    );
}
